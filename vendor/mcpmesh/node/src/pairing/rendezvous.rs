//! The pairing rendezvous over ALPN `mcpmesh/pair/1`. GATE-EXEMPT by design: a pairing peer is
//! by definition not yet in the allowlist; it is authenticated by possession of the invite
//! secret, not by the trust gate. This module holds BOTH sides: [`handle_inviter_side`] (the
//! accept-time handler) and [`redeem_invite`] (the dialer).
//!
//! **The two writes that make a pairing functional (the load-bearing fact).**
//! Admitting a paired peer to a service needs TWO independent facts on the inviter:
//!  1. a [`PeerEntry`] `{ endpoint_id → nickname }` so the [`AllowlistGate`] RESOLVES the peer's
//!     mesh dial to its nickname (identity/trust); and
//!  2. the peer's nickname in the service's config `[services.<svc>].allow`, so `select_service`
//!     ADMITS that resolved nickname (authorization) — this allow is baked into the [`Services`]
//!     snapshot at `build_services` time, so it takes effect only after a RELOAD.
//!
//! A [`PeerEntry`] alone leaves the peer KNOWN-BUT-FORBIDDEN. [`handle_inviter_side`]
//! writes (1) then calls the [`InviterCtx::grant`] hook for (2) — see the success arm below.
//!
//! **Asymmetric grant.** `invite notes` gives the REDEEMER access to `notes` and
//! gives the INVITER a dial-back entry with NO service grants. So:
//!
//!  - the redeemer's alice-entry has `services = invite.services` (what the redeemer may DIAL);
//!  - the inviter's bob-entry has `services = []` (a dial-back identity row — the inviter may
//!    dial nothing on the redeemer). `PeerEntry.services` is a client-side DIRECTORY of what to
//!    dial, never an authorization input (nothing reads it for admission), so the `[]` here is
//!    semantic cleanliness — but it is the correct encoding of the asymmetry.
//!
//! **Second pairings MERGE, never clobber.** `PeerStore::add` is a replace-on-endpoint_id upsert
//! (a contract other callers rely on), so BOTH rendezvous write sites resolve-then-merge before
//! adding: the redeemer UNIONs a repeat grant into its dial directory and takes the new invite's
//! suggested nickname (rename-by-fresh-invite); the inviter PRESERVES its stored nickname + dial
//! directory (a reverse pairing must not wipe what an earlier redeem granted us) — and neither
//! side ever downgrades a verified `user_id` to `None`, nor a stored `last_addr` (a fresh
//! pairing REFRESHES the dial hint; a merge never replaces `Some` with `None`). See the
//! per-site comments for the rules.
//!
//! This module deliberately never sees the daemon's state: the inviter side runs against the
//! narrow [`InviterCtx`] the daemon assembles (peer store + invite ring + the grant hook), so
//! pairing can be read and tested on its own.
//!
//! [`AllowlistGate`]: crate::allowlist::AllowlistGate
//! [`Services`]: mcpmesh_net::Services
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, bail};
use tokio::io::BufReader;

use mcpmesh_local_api::PairResult;
use mcpmesh_net::framing::{FrameReader, Inbound, write_frame};

use crate::allowlist::{PeerEntry, PeerStore};
use crate::pairing::sas::short_auth_code;
use crate::pairing::{Invite, LiveInvites, Redeem, SelfEnroll};
use crate::util::epoch_now_u64 as epoch_now;

/// Frame cap for the pair rendezvous. The redeemer's hello is a tiny JSON object (two 32-byte
/// arrays + a short nickname), so a small cap is ample and bounds a hostile stranger's frame
/// (the pair ALPN accepts strangers by design).
const MAX_PAIR_FRAME: usize = 64 * 1024;

/// Generic wire refusal reason. Deliberately does NOT distinguish unknown-vs-expired-vs-wrong
/// secret: a specific reason would be a redemption oracle an attacker could probe. The specific [`Redeem`] variant is logged SERVER-side only. A malformed frame and an
/// id mismatch get their own reasons — neither is a secret oracle.
const REASON_REFUSED: &str = "pairing refused";

/// The reason on an [`RefusalCode::IdentityRevoked`] refusal (#218). Says exactly what the
/// revocation refuses — a device PRESENTING that identity — and who can lift it. It does not claim
/// another invite "will not help": identity revocation cannot stop a person who stops presenting
/// the key (no binding, or a new user key, lands as an ordinary `eid:` pairing), and a sentence
/// promising otherwise would be the overclaim this reason exists to avoid.
const REASON_IDENTITY_REVOKED: &str = "your identity is revoked on the inviter, which refuses any \
                                       device presenting it — only the inviter can lift that \
                                       (peer_unrevoke)";
const REASON_MALFORMED: &str = "malformed request";
const REASON_ID_MISMATCH: &str = "id mismatch";

/// The accept gate's fast-close reason when NO invite is live (#87b) — ONE constant for both
/// sides: the daemon's `ALPN_PAIR` accept arm writes it, [`redeem_invite`] matches it off
/// `close_reason()` and turns it into an actionable error instead of a bare connection failure.
pub(crate) const NO_LIVE_INVITE_CLOSE: &[u8] = b"no pairing in progress";

/// The distinguishable nickname-collision refusal (#87). Only ever sent to a caller that proved
/// possession of a live secret (the peek pre-check) or spent one (the post-redeem race guard) —
/// never to an unproven dialer, or it becomes a store-contents oracle. `invite_survived` selects
/// the recovery guidance: the pre-check path preserves the invite, the race-guard path burned it.
///
/// The redeemer carries this string VERBATIM into [`NicknameTaken`] (under a `pairing refused: `
/// prefix) rather than rebuilding the sentence, so there is one source for the wording — the
/// inviter is also the only side that knows whether the invite survived.
///
/// **Names the action, never a control verb (#147).** The recovery clause used to say "pick a
/// different nickname (`set_nickname`)". That verb is control-API vocabulary a GUI user cannot
/// type, see, or find — and because this string is built INVITER-side and travels to the redeemer,
/// the embedder that displays it could not rewrite it into its own words without substring-matching
/// our prose. The sibling clause "ask the inviter for a fresh invite" was already the model: it
/// names an action. An embedder wanting its own copy should branch on
/// [`ERR_NICKNAME_TAKEN`](mcpmesh_local_api::ERR_NICKNAME_TAKEN) instead of reading this at all.
fn reason_nickname_taken(nickname: &str, invite_survived: bool) -> String {
    let recovery = if invite_survived {
        "the invite was NOT consumed — rename this node and redeem the same invite again"
    } else {
        "ask the inviter for a fresh invite"
    };
    format!("nickname '{nickname}' is already taken by another paired peer; {recovery}")
}

/// The complete nickname-collision refusal: the prose AND its code, chosen together (#147).
///
/// Both send sites go through here rather than building a `PairReply` each, because the two are
/// ONE decision. The code means "rename and redeem the SAME invite again", so it is exactly the
/// `invite_survived` case — and the first implementation of #147 stamped it on both sites, which
/// would have had an embedder send a race-guard loser back to an invite that no longer exists.
///
/// A test over a helper could not have caught that: the bug was at the call site. Keeping the two
/// fields inseparable is what makes it unrepresentable.
fn collision_refusal(nickname: &str, invite_survived: bool) -> PairReply {
    PairReply::Refused {
        reason: reason_nickname_taken(nickname, invite_survived),
        code: invite_survived.then_some(RefusalCode::NicknameTaken),
    }
}

/// A machine-readable refusal kind on [`PairReply::Refused`] (#147), so the REDEEMER can raise a
/// typed error without parsing the inviter's prose — the same anti-pattern we are asking embedders
/// to stop doing, and it would break the moment we improved the wording.
///
/// Daemon-to-daemon only; the control API sees the mapped
/// [`ERR_NICKNAME_TAKEN`](mcpmesh_local_api::ERR_NICKNAME_TAKEN) instead.
///
/// **Deliberately narrow.** It rides only refusals that are already distinguishable and already
/// sent exclusively to a caller that proved possession of a live secret: the nickname collision,
/// and (#218) a revoked identity. The generic [`REASON_REFUSED`] path gains NO code: it withholds
/// unknown-vs-expired-vs-wrong-secret on purpose, and labelling it would build the redemption
/// oracle that reason exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum RefusalCode {
    /// The redeemer's nickname is held by a DIFFERENT paired peer AND the invite survived, so
    /// renaming and redeeming the same invite again works (#87).
    ///
    /// **The surviving invite is part of the meaning, not a coincidence.** It is what the remedy
    /// every consumer writes off this code depends on. The post-redeem race guard refuses the
    /// same collision with the invite already BURNED, and deliberately sends no code: an embedder
    /// branching on one would tell the user to retry an invite that is gone.
    NicknameTaken,
    /// The redeemer's PROVEN `user_id` is one the inviter has revoked (`peer_revoke b64u:`), so no
    /// row is written and no grant is made (#218). Sent after the burn, to a caller that proved a
    /// live secret AND a binding for that identity — it learns nothing it did not already hold.
    /// The remedy is the inviter's `peer_unrevoke` — another invite redeemed presenting the same
    /// identity is refused the same way — which is why the redeemer maps it to its own control code
    /// rather than [`ERR_INVITE_REFUSED`]'s "ask for another".
    IdentityRevoked,
    /// A refusal kind this node predates. Never sent — only reached on receive.
    Unknown,
}

/// Hand-written so a refusal kind from a NEWER inviter lands on [`RefusalCode::Unknown`] instead of
/// failing the whole reply, which would turn an informative refusal into an opaque parse error on a
/// pinned redeemer. Same reasoning (and same shape) as `ReachabilitySource` in #150; accepts any
/// value, not just an unrecognized string, since `#[serde(default)]` covers an absent key alone.
impl<'de> serde::Deserialize<'de> for RefusalCode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct AnyCode;

        impl<'de> serde::de::Visitor<'de> for AnyCode {
            type Value = RefusalCode;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a refusal code")
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Self::Value, E> {
                Ok(match s {
                    "nickname_taken" => RefusalCode::NicknameTaken,
                    "identity_revoked" => RefusalCode::IdentityRevoked,
                    _ => RefusalCode::Unknown,
                })
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(RefusalCode::Unknown)
            }

            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(RefusalCode::Unknown)
            }

            fn visit_some<D: serde::Deserializer<'de>>(
                self,
                d: D,
            ) -> Result<Self::Value, D::Error> {
                d.deserialize_any(AnyCode)
            }

            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(RefusalCode::Unknown)
            }

            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(RefusalCode::Unknown)
            }

            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(RefusalCode::Unknown)
            }

            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(RefusalCode::Unknown)
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut m: A,
            ) -> Result<Self::Value, A::Error> {
                // Drained deliberately: answering without consuming desynchronizes the parser and
                // fails the enclosing reply, which is the failure this impl exists to avoid.
                while m
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(RefusalCode::Unknown)
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut s: A,
            ) -> Result<Self::Value, A::Error> {
                while s.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(RefusalCode::Unknown)
            }
        }

        d.deserialize_any(AnyCode)
    }
}

/// A pairing refusal that carries its own JSON-RPC code (#159).
///
/// One type rather than a marker struct per condition: `respond` downcasts once and reads `.code`,
/// so adding the seventh onboarding condition is a constant plus a call site, not another arm.
/// Scoped to the ceremony family, not the `pair` verb: `invite { as_self }` and
/// `self_enroll_detach` raise one too (#214).
///
/// The point is that an embedder can decide PER CASE whether to render our prose or replace it.
/// Before this, `ERR_NICKNAME_TAKEN` was the only coded pairing failure, so the choice was
/// all-or-nothing: forward every sentence verbatim to end users, or substring-match them.
#[derive(Debug)]
pub struct PairRefusal {
    code: i64,
    message: String,
}

impl PairRefusal {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The JSON-RPC code `respond` should answer with.
    pub fn code(&self) -> i64 {
        self.code
    }
}

impl std::fmt::Display for PairRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PairRefusal {}

/// The redeemer-side typed error for a nickname-collision refusal (#147), which `respond` downcasts
/// to [`ERR_NICKNAME_TAKEN`](mcpmesh_local_api::ERR_NICKNAME_TAKEN). Same shape as
/// [`NoSuchService`](crate::daemon::NoSuchService): a type, a downcast, a stable code.
///
/// It carries the inviter's reason VERBATIM rather than rebuilding the sentence: the inviter is the
/// side that knows whether the invite survived, and re-deriving that here would be a second source
/// of truth for a string this issue exists to make single-sourced.
#[derive(Debug)]
pub struct NicknameTaken(pub String);

impl std::fmt::Display for NicknameTaken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NicknameTaken {}

/// Did the inviter close this connection with the accept gate's no-live-invite reason (#87b)?
/// The `redeem_invite` mirror of the #89 probe-throttle detection: read off the CONNECTION, not
/// parsed out of whichever stream error surfaced first.
fn no_live_invite_close(conn: &iroh::endpoint::Connection) -> bool {
    matches!(
        conn.close_reason(),
        Some(iroh::endpoint::ConnectionError::ApplicationClosed(ac))
            if ac.reason.as_ref() == NO_LIVE_INVITE_CLOSE
    )
}

/// The shared #87 collision check: resolve any EXISTING entry for the TLS-authenticated
/// redeemer id, and whether its self-asserted nickname collides with a DIFFERENT stored peer.
/// ONE helper for the pre-burn check and the post-redeem race guard, so the two cannot drift.
/// Blocking (redb read) → spawn_blocking.
async fn resolve_and_check_collision(
    store: &Arc<PeerStore>,
    nickname: &str,
    tls_id: [u8; 32],
) -> anyhow::Result<(Option<PeerEntry>, bool)> {
    let store_c = store.clone();
    let nickname_c = nickname.to_string();
    tokio::task::spawn_blocking(move || {
        let existing = store_c.resolve(&tls_id)?;
        let collides = existing.is_none() && nickname_collision(&store_c, &nickname_c, &tls_id)?;
        anyhow::Ok((existing, collides))
    })
    .await
    .context("join nickname collision check")?
}

/// Choose the collision refusal, given whether OUR OWN alias is what collided (#87 gate).
///
/// Two different failures wear the same shape and must not wear the same reply:
///
/// - **No alias.** The colliding name is the one the redeemer claimed for itself. It can rename and
///   retry, so name it and answer [`RefusalCode::NicknameTaken`] — the documented
///   rename-and-retry code.
/// - **Alias set.** The colliding name is OURS, chosen on OUR machine, over a name the redeemer
///   cannot see or influence. The first draft interpolated it into the reply, which sent the
///   inviter's private name for that peer — and when the clash was with a third party, disclosed
///   that name too. Worse, it carried `NicknameTaken`, so an embedder following the documented
///   contract would rename and retry forever: every attempt collides identically, because the name
///   is not theirs to change. It answers with the GENERIC refusal instead — byte-identical to every
///   other opaque one, so it discloses nothing, not even that a collision is what happened. The
///   operator gets the detail in the server-side log line.
fn collision_reply(alias: Option<&str>, hello: &RedeemerHello, invite_survived: bool) -> PairReply {
    match alias {
        None => collision_refusal(&hello.redeemer_nickname, invite_survived),
        Some(_) => PairReply::Refused {
            reason: REASON_REFUSED.into(),
            code: None,
        },
    }
}

/// The name the inviter will actually store for a redeemer (#87).
///
/// The inviter's own `peer_nickname` alias when the invite carries one, else the redeemer's
/// self-claim. ONE function, used by the collision pre-check, the post-burn race guard, and the
/// store write — so the name that is checked and the name that is written cannot diverge. They
/// diverging is the whole failure mode: a check against a name nobody stores proves nothing.
fn effective_redeemer_nickname<'a>(alias: Option<&'a str>, claimed: &'a str) -> &'a str {
    alias.unwrap_or(claimed)
}

/// The redeemer's first (and only) frame: the secret it is redeeming plus its self-claimed id
/// and suggested nickname. `[u8; 32]` fields serde-round-trip as JSON arrays (same as `Invite`).
/// The claimed `redeemer_id` is NOT trusted — the TLS-authenticated `conn.remote_id()` is
/// authoritative and must match it.
#[derive(serde::Serialize, serde::Deserialize)]
struct RedeemerHello {
    secret: [u8; 32],
    redeemer_id: [u8; 32],
    redeemer_nickname: String,
    /// Optional self-sovereign identity: the redeemer's user public key (`b64u`) and a device→user
    /// binding signature over ITS OWN endpoint (`b64u`), proving this device belongs to that user
    /// (`mcpmesh_trust::binding`). `#[serde(default)]` so a peer with no user key OMITS them
    /// (backward-compatible) and the inviter stores the entry with `user_id: None`. NEVER trusted
    /// unverified — the inviter re-verifies the binding against the TLS-authenticated `redeemer_id`.
    #[serde(default)]
    user_pk: Option<String>,
    #[serde(default)]
    binding_sig: Option<String>,
    /// DEVICE ATTESTATION (#85 ask 3): this is not an invite redemption. The caller is another
    /// device of a person the receiver ALREADY pairs with, proving it by the binding above.
    ///
    /// `secret` is ignored on this path — there is no invite — so `user_pk`/`binding_sig` become
    /// REQUIRED rather than optional, and the receiver verifies the binding against the
    /// TLS-authenticated endpoint id exactly as it does on the pairing path.
    ///
    /// `#[serde(default)]` so an older redeemer's hello (which never sets it) is an ordinary
    /// redemption, and an older RECEIVER simply ignores the field and refuses the zero secret —
    /// which is the correct outcome for a node that cannot honour the ceremony.
    #[serde(default)]
    attest: bool,
}

/// The inviter's reply. On success it carries the inviter's identity so the redeemer can write
/// its dial-back entry; on failure a generic reason.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum PairReply {
    Ok {
        inviter_id: [u8; 32],
        inviter_nickname: String,
        /// The inviter's optional self-sovereign identity — same shape/semantics as
        /// [`RedeemerHello`]'s, verified by the redeemer against the invite's `inviter_id`.
        #[serde(default)]
        user_pk: Option<String>,
        #[serde(default)]
        binding_sig: Option<String>,
    },
    Refused {
        reason: String,
        /// The machine-readable refusal kind (#147), so the redeemer raises a typed error instead
        /// of parsing `reason`. Additive: an inviter older than 0.25.1 sends none, and the
        /// redeemer falls back to today's generic error — a mixed-version pairing still refuses
        /// correctly, just without the branchable code. `None` on every refusal that is
        /// deliberately opaque (see [`RefusalCode`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<RefusalCode>,
    },
}

/// This daemon's own self-sovereign identity presentation for a pairing exchange: its user public
/// key and a device→user binding signature over ITS OWN endpoint (both `b64u`), precomputed once at
/// serve time from the daemon's [`UserKey`](mcpmesh_trust::UserKey) via
/// [`binding::present`](mcpmesh_trust::binding::present). A `None` at a call site means this daemon
/// has no user key and presents no identity, so the peer stores `user_id: None` — exactly how a
/// pre-identity peer is stored.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SelfBinding {
    pub user_pk: String,
    pub sig: String,
}

/// The inviter-side AUTHORIZATION hook: `(principal, display_nickname, services)` → append the
/// redeemer's STABLE principal (#38: its verified `b64u:` user_id when it presented a binding,
/// else its `eid:` device principal — never the rewritable display nickname) to each granted
/// service's config `allow` and hot-reload the serving registry so the peer is actually
/// admitted. The display nickname rides along for the audit/log lines only. Boxed so this
/// module never depends on the daemon's config/reload machinery — the daemon hands the hook in
/// via [`InviterCtx`].
pub type GrantFn = Box<
    dyn Fn(String, String, Vec<String>) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>
        + Send
        + Sync,
>;

/// The redeemer-side MUTUAL grant hook (#43): `(inviter_principal, inviter_display)` → grant
/// the inviter access to ALL services THIS node serves. Symmetric with [`GrantFn`] (the
/// inviter side); the daemon supplies it (`None` in tests, which assert only the store write).
pub type GrantBackFn = Box<
    dyn Fn(String, String) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>
        + Send
        + Sync,
>;

/// The ceremony-surface hook: park the completed pairing where `status` can show the inviter's
/// human the short authentication code. Display-only state, never a trust input. The whole row,
/// so the arm that ran the ceremony sets `self_enroll` (#214) — the ring must not infer it.
pub type RecordPairingFn = Box<dyn Fn(mcpmesh_local_api::RecentPairing) + Send + Sync>;

/// Everything the inviter-side rendezvous needs from the daemon hosting it — the narrow seam that
/// keeps this module free of daemon state. The daemon assembles one per accepted pair connection
/// (`MeshState::inviter_ctx`); tests can assemble one from parts.
///
/// **Reentrancy (why [`grant`](Self::grant) may reload the accept loop that spawned this
/// handler).** The handler runs as a DETACHED child `tokio::spawn` of the accept loop (spawned
/// per-connection). The grant hook aborts the OLD accept-loop task and spawns a NEW one —
/// aborting a `JoinHandle` aborts only THAT task, never its already-spawned children, so the
/// handler keeps running and finishes its reply over the still-live connection. The daemon's
/// reload lock serializes the grant against every other config mutation; the handler holds no
/// daemon lock when it invokes the hook. No self-abort, no deadlock.
pub struct InviterCtx {
    /// The peer allowlist store (the same open database the live trust gate reads).
    pub store: Arc<PeerStore>,
    /// The in-RAM ring of outstanding invites the redeemed secret is looked up in.
    pub invites: Arc<LiveInvites>,
    /// The daemon's config path — read (not written) by the nickname-collision guard.
    pub config_path: PathBuf,
    /// This daemon's own identity presentation, if it has a user key.
    pub self_binding: Option<SelfBinding>,
    /// The authorization hook (see [`GrantFn`]).
    pub grant: GrantFn,
    /// The ceremony-surface hook (see [`RecordPairingFn`]).
    pub record_pairing: RecordPairingFn,
    /// #86: record a durable trust event. `(event, target)`.
    pub audit_trust: AuditTrustFn,
    /// #86: sign a device→user binding for another device of THIS person, given that device's
    /// TLS-authenticated endpoint id. `None` when this daemon has no user key — there is then no
    /// identity to enroll into, and a self-enrollment is refused rather than silently completing.
    /// Also `None` when this node's identity changed since [`self_binding`](Self::self_binding) was
    /// snapshotted, or when the key on disk is not the one behind that binding's `user_pk` (#221).
    /// The daemon's implementation checks both under its user-key lock before signing, so a
    /// `Some` verifies under the `user_pk` the reply presents.
    ///
    /// A hook rather than the key itself, so this module never learns where the key lives.
    pub sign_binding: SignBindingFn,
    /// #85 ask 3: does this node admit another DEVICE of a person it already pairs with, on the
    /// strength of a user-key binding? `[identity].admit_attested_devices`, OFF by default.
    ///
    /// A field rather than a config read, so the un-offered ceremony is unrepresentable at this
    /// seam — the same discipline #178 established for `SelfEnroll`, and for the same reason: every
    /// caller has to say which ceremonies it is willing to complete.
    pub admit_attested: bool,
    /// This node's OWN endpoint id (#85 ask 3). The pairing path takes it from the invite
    /// (`invite.inviter_id`); an attestation has no invite, and the attesting device — freshly
    /// restored from a recovery phrase — holds no row for us, so the reply has to carry it.
    pub self_endpoint_id: [u8; 32],
    /// This node's OWN nickname (#85 ask 3). The pairing path takes it from the invite; an
    /// attestation has none, and the reply has to tell the attesting device what to file us under.
    pub self_nickname: String,
}

/// Persist an adopted self-enrollment binding (#86) — the ONLY copy, since the enrolled device
/// cannot re-derive it (it holds no user key).
pub type AdoptBindingFn = Box<
    dyn Fn(SelfBinding) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync,
>;

/// See [`InviterCtx::audit_trust`] (#86).
pub type AuditTrustFn = Box<dyn Fn(String, Option<String>) + Send + Sync>;

/// See [`InviterCtx::sign_binding`] (#86). `endpoint_id` → the `b64u:` signature, or `None`.
///
/// Async (#221): the daemon's implementation re-checks this node's identity under its user-key
/// lock and reads the key on a blocking thread. It performs no network IO.
pub type SignBindingFn =
    Box<dyn Fn([u8; 32]) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

/// Verify a peer's OPTIONAL presented binding against the TLS-authenticated peer id, returning the
/// peer's proven `user_id` if — and only if — it presented a binding that verifies. Absent fields →
/// `None` (a backward-compatible pre-binding peer). A PRESENT-but-INVALID binding is rejected (a
/// `warn` + `None`): a peer asserting a `user_id` must PROVE ownership of that user key AND that the
/// binding is for its authenticated endpoint (`binding::verify_presented`'s two invariants), so an
/// unprovable id is never stored. It does not FAIL the pairing — identity is ADDITIVE to the nickname
/// trust grant, and an invalid binding conveys no privilege (it cannot forge a `user_id`), so the
/// pairing still succeeds with `user_id: None` rather than burning the invite on a crypto hiccup.
fn verified_user_id(
    user_pk: &Option<String>,
    binding_sig: &Option<String>,
    authenticated_id: &[u8; 32],
) -> Option<String> {
    match (user_pk, binding_sig) {
        (Some(pk), Some(sig)) => {
            match mcpmesh_trust::binding::verify_presented(pk, sig, authenticated_id) {
                Ok(uid) => Some(uid),
                Err(e) => {
                    tracing::warn!(
                        %e,
                        "peer presented an invalid device->user binding; storing entry without a user_id"
                    );
                    None
                }
            }
        }
        // No binding presented (or a half-presented one) — no self-sovereign id to store.
        _ => None,
    }
}

/// Inviter-side handler for one inbound pair connection. The redeemer opens a bi-stream and
/// sends a `RedeemerHello`; we verify the EndpointId binding (the TLS-authenticated id must
/// match the claimed one), redeem the secret against the live registry, and on success write
/// the [`PeerEntry`] trust grant, GRANT service authorization ([`InviterCtx::grant`]), reply
/// with our identity, and log the short authentication code (SAS). Every attempt is logged; no
/// peer EndpointId is ever logged (the surface discipline: porcelain and logs speak nicknames).
///
/// Takes an [`InviterCtx`]: the redeem reads `ctx.invites` + `ctx.store`, and the authorization
/// grant runs through the `ctx.grant` hook the daemon supplied (see the [`InviterCtx`] doc for
/// the reload-reentrancy argument).
/// What an accepted attestation will write. Separated from the I/O so the DECISION is testable.
#[derive(Debug, PartialEq, Eq)]
struct AdmitPlan {
    user_id: String,
    nickname: String,
    services: Vec<String>,
}

/// The whole authorization decision for an attestation, as a pure-ish function over the store.
///
/// Extracted from [`handle_attestation`] because the property that matters most here — that the
/// binding is verified against the **TLS-authenticated** endpoint and never the id the caller wrote
/// in its own hello — cannot be reached end to end: `attest_to` always sends its real id, so no
/// honest client can express the attack. Swapping `tls_id` for `hello.redeemer_id` went UNCAUGHT by
/// every e2e test until this seam existed.
///
/// The `Err` is a reason for the LOG only. Every refusal goes back on the wire identically, because
/// the differences between them are exactly the probe an unproven caller would use to enumerate who
/// this node pairs with.
fn attestation_decision(
    hello: &RedeemerHello,
    tls_id: [u8; 32],
    ctx: &InviterCtx,
) -> Result<AdmitPlan, &'static str> {
    if !ctx.admit_attested {
        return Err("not enabled on this node");
    }
    let (Some(user_pk), Some(sig)) = (hello.user_pk.as_deref(), hello.binding_sig.as_deref())
    else {
        return Err("no binding presented");
    };
    // `tls_id`, ALWAYS — never `hello.redeemer_id`, which the caller writes. `handle_inviter_side`
    // also refuses a hello whose claimed id disagrees, so today the two are equal here; using the
    // authenticated one anyway keeps that check from being load-bearing for a SECOND property, and
    // means this function is correct in isolation.
    let user_id = mcpmesh_trust::binding::verify_presented(user_pk, sig, &tls_id)
        .map_err(|_| "binding does not verify against the authenticated endpoint")?;
    if ctx.store.is_revoked(&tls_id) {
        return Err("endpoint is revoked");
    }
    // …and the IDENTITY, which is the check that actually protects a stolen laptop.
    //
    // Endpoint revocation was the documented remedy until the 0.46.0 gate disproved it end to end:
    // a thief holding the stolen machine holds the USER KEY — that is exactly what
    // `identity export`/`import` moves — so they mint a brand-new endpoint id, sign a fresh binding
    // over it, and the endpoint check never sees an id it knows. The admitted device opened a live
    // session in the probe. Attestation authorizes on the PERSON, so the refusal has to be
    // available at that granularity too.
    if ctx.store.is_user_revoked(&user_id) {
        return Err("identity is revoked");
    }
    let existing = ctx
        .store
        .entries_for_user(&user_id)
        .map_err(|_| "peer store read failed")?;
    if existing.is_empty() {
        return Err("no existing row for that identity");
    }
    // INTERSECTION across the person's rows: a new device must never arrive holding the most
    // privileged grant on the node.
    let mut services: Vec<String> = existing[0].services.clone();
    for e in &existing[1..] {
        services.retain(|s| e.services.contains(s));
    }
    Ok(AdmitPlan {
        user_id,
        nickname: existing[0].nickname.clone(),
        services,
    })
}

/// #85 ask 3 — admit another DEVICE of a person we already pair with, on the strength of a
/// user-key binding rather than a fresh SAS ceremony.
///
/// A `b64u:` user id is what peers pin, and `PeerEntry.user_id` is written once at pairing and never
/// refreshed — so a replacement machine holding the right user key (restored from 0.42.0's recovery
/// phrase) was a complete stranger to every peer, and recovery meant an in-person ceremony with
/// everyone you had ever paired with.
///
/// The order of the checks below is the design. Each one refuses generically; a caller learns only
/// that it was refused, never which check said so, because the differences are exactly the
/// probe an unproven caller would use to enumerate who we pair with.
///
/// 1. **The receiver opted in.** Off by default — see `[identity].admit_attested_devices`.
/// 2. **The binding verifies against the TLS-AUTHENTICATED endpoint** (`verify_presented`), never a
///    self-asserted id. A binding transplanted from another device fails here.
/// 3. **The endpoint is not REVOKED** (#85 ask 4). Checked before anything is written, and the
///    reason ask 4 shipped first: without it a device someone had declared stolen could walk back
///    in holding a binding it still had.
/// 4. **We already hold a row for that `user_id`.** This is the authorization in full: attestation
///    admits another device of someone we paired with, and is not itself a pairing mechanism. An
///    unknown `user_id` is refused.
///
/// The new row carries the person's `user_id` and nickname — it is the same person, and nicknames
/// here are per-person, not per-device — and the INTERSECTION of that person's existing service
/// grants. Never the union: a new device must not arrive holding the most-privileged grant on the
/// node, and the operator can widen it deliberately with `service_allow_grant`.
async fn handle_attestation(
    send: &mut iroh::endpoint::SendStream,
    hello: &RedeemerHello,
    tls_id: [u8; 32],
    ctx: &InviterCtx,
) -> anyhow::Result<()> {
    let refuse_generic = async |send: &mut iroh::endpoint::SendStream| {
        let _ = send_reply(
            send,
            &PairReply::Refused {
                reason: REASON_REFUSED.into(),
                code: None,
            },
        )
        .await;
        anyhow::Ok(())
    };

    let plan = match attestation_decision(hello, tls_id, ctx) {
        Ok(plan) => plan,
        Err(why) => {
            tracing::debug!(%why, "refused a device attestation");
            return refuse_generic(send).await;
        }
    };
    let AdmitPlan {
        user_id,
        nickname,
        services,
    } = plan;

    // Same merge discipline on this side (#85 ask 3 gate): a re-attestation by a device we already
    // hold must not wipe its dial hint or its pairing stamp.
    let prior = ctx.store.resolve(&tls_id)?;
    ctx.store.add(PeerEntry {
        endpoint_id: tls_id,
        nickname: nickname.clone(),
        services: services.clone(),
        paired_at: prior
            .as_ref()
            .and_then(|e| e.paired_at.clone())
            .or_else(|| Some(epoch_now().to_string())),
        user_id: Some(user_id.clone()),
        last_addr: prior.as_ref().and_then(|e| e.last_addr.clone()),
    })?;

    // A device joining under a known identity is NEVER silent: durable audit + the ceremony ring
    // that feeds `status.recent_pairings` and the subscribe stream.
    (ctx.audit_trust)(
        "device_attest".into(),
        Some(mcpmesh_net::EndpointId::from_bytes(tls_id).principal()),
    );
    tracing::warn!(
        peer = %mcpmesh_net::EndpointId::from_bytes(tls_id).principal(),
        %user_id,
        nickname = %nickname,
        services = ?services,
        "admitted a new DEVICE of a person already paired with (attestation)"
    );

    let reply = PairReply::Ok {
        inviter_id: ctx.self_endpoint_id,
        // OUR name for ourselves — the same field the pairing path fills from `invite.nickname`.
        //
        // It carried `plan.nickname` (our name for THEIR person) until the 0.46.0 gate: the
        // attesting device then filed us under its own owner's name, `mcpmesh attest to` printed
        // "Admitted by bob." on bob's own machine, and attesting to several peers landed all of
        // them under one nickname — a real collision, since `entry_for` is first-match and
        // `remove` deletes every match.
        inviter_nickname: ctx.self_nickname.clone(),
        user_pk: ctx.self_binding.as_ref().map(|b| b.user_pk.clone()),
        binding_sig: ctx.self_binding.as_ref().map(|b| b.sig.clone()),
    };
    send_reply(send, &reply).await?;
    Ok(())
}

pub async fn handle_inviter_side(
    conn: iroh::endpoint::Connection,
    ctx: InviterCtx,
) -> anyhow::Result<()> {
    // The redeemer opens the bi-stream; we accept it. `accept_bi` resolves once the redeemer
    // has sent its first bytes (the hello).
    let (mut send, recv) = conn.accept_bi().await?;
    let mut reader = FrameReader::new(BufReader::new(recv), MAX_PAIR_FRAME);

    // Read exactly one hello frame. A framing violation, an EOF, or a JSON that is not a
    // RedeemerHello → refuse (best-effort) and return; the connection is not a valid redeemer.
    let hello: RedeemerHello = match reader.next().await? {
        Some(Inbound::Frame(v)) => match serde_json::from_value(v) {
            Ok(h) => h,
            Err(_) => return refuse(&mut send, REASON_MALFORMED, "malformed hello").await,
        },
        _ => return refuse(&mut send, REASON_MALFORMED, "malformed hello").await,
    };

    // EndpointId binding: `conn.remote_id()` is the TLS-authenticated redeemer id and is
    // AUTHORITATIVE — a redeemer cannot lie about its own id. Reject a hello whose claimed id
    // disagrees, and use the TLS id (NOT the message field) everywhere below.
    let tls_id = *conn.remote_id().as_bytes();
    if tls_id != hello.redeemer_id {
        return refuse(&mut send, REASON_ID_MISMATCH, "id mismatch").await;
    }

    let now = epoch_now();

    // #85 ask 3 — DEVICE ATTESTATION. Handled before any invite lookup, because this path has no
    // invite: the caller is another device of a person we ALREADY pair with, and the binding it
    // presents is the whole credential.
    if hello.attest {
        return handle_attestation(&mut send, &hello, tls_id, &ctx).await;
    }

    // #87: collision pre-check BEFORE the burn — but ONLY behind a live-secret peek. Order is
    // the whole design: checking the nickname first for every caller would let a stranger with
    // a garbage secret probe which names exist in the store, so the unproven path must stay on
    // the generic `try_redeem` refusals below, byte-for-byte. A caller that proves possession
    // of a live secret may be told the truth: the name is taken, the invite was NOT consumed,
    // rename and redeem it again — which turns two same-hostname machines' first pairing from
    // a burned invite plus a generic refusal into a self-service retry.
    // #86: skip for a self-enrollment — it stores no nickname on either side, so refusing over a
    // name neither party will write is nonsense, and its remedy ("rename and redeem again") is
    // advice for a problem the caller does not have.
    let self_enrolling = ctx.invites.peek_is_self(&hello.secret, now);
    if !self_enrolling && let Some(alias) = ctx.invites.peek_live_alias(&hello.secret, now) {
        // #87: check the name we will actually STORE. With a `peer_nickname` on the invite, the
        // redeemer's self-claim is never used, so checking it here would refuse over a name we
        // were never going to write — and admit over the one we were.
        let claimed = effective_redeemer_nickname(alias.as_deref(), &hello.redeemer_nickname);
        let (_, collides) = resolve_and_check_collision(&ctx.store, claimed, tls_id).await?;
        if collides {
            // Logged SERVER-side with the nickname (a pairing artifact, not a surface leak) —
            // NO endpoint id, NO secret.
            tracing::warn!(
                nickname = %claimed,
                aliased = alias.is_some(),
                "pairing refused: nickname collision (invite preserved)"
            );
            let _ = send_reply(&mut send, &collision_reply(alias.as_deref(), &hello, true)).await;
            return Ok(());
        }
    }

    match ctx.invites.try_redeem(&hello.secret, now).await {
        // A self-invite must ALSO be single-use HERE, not only at mint. `Invite` is `pub` with `pub`
        // fields and `LiveInvites::mint` is `pub`, so a hand-edited invite file or an embedder
        // building one directly can present a multi-use identity invite — the standing offer to
        // become this person that the mint guard exists to refuse. The repo's own `checked_sub`
        // burn establishes the discipline: an invariant on a bearer credential fails closed
        // WHEREVER it is violated (#86 gate).
        Redeem::Ok(invite) if invite.as_self && invite.uses_remaining > 0 => {
            tracing::warn!("refusing a MULTI-USE self-enrollment invite");
            let _ = send_reply(
                &mut send,
                &PairReply::Refused {
                    reason: REASON_REFUSED.into(),
                    code: None,
                },
            )
            .await;
            Ok(())
        }
        Redeem::Ok(invite) if invite.as_self => {
            // #86 SELF-ENROLLMENT. The redeemer is another DEVICE of this person, so the whole
            // peer-row/grant path below is skipped: writing a row would put this person in their
            // own contact list and — worse — make their own second device an authorizable
            // principal in their own allow lists.
            //
            // What we hand over is a device→user binding for the redeemer's TLS-AUTHENTICATED
            // endpoint, signed with our user key. The KEY NEVER MOVES: `present` signs an arbitrary
            // endpoint id, so the new device can present this signature as its own and every peer
            // resolves both devices to one `user_id`.
            //
            // Signed for `tls_id`, never a self-asserted id: a binding for an endpoint the redeemer
            // does not control would be useless to them and dangerous to issue.
            let Some(self_binding) = ctx.self_binding.as_ref() else {
                // No user key here means there is no identity to enroll INTO. Refuse rather than
                // complete a ceremony that silently achieves nothing.
                let _ = send_reply(
                    &mut send,
                    &PairReply::Refused {
                        reason: "this device has no user identity to enroll into".into(),
                        code: None,
                    },
                )
                .await;
                return Ok(());
            };
            let sig = match (ctx.sign_binding)(tls_id).await {
                Some(sig) => sig,
                None => {
                    let _ = send_reply(
                        &mut send,
                        &PairReply::Refused {
                            reason: REASON_REFUSED.into(),
                            code: None,
                        },
                    )
                    .await;
                    return Ok(());
                }
            };
            // #86 gate: enrollment mints IDENTITY irrevocably, so it must leave a DURABLE record —
            // `record_pairing` below is the display ring, documented as "lost on restart, NOT trust
            // data". `pair`/`unpair`/`roster_install` all audit; this was the highest-value trust
            // act in the system and the only one that did not. Exactly the #65 finding, recurring.
            (ctx.audit_trust)(
                "self_enroll".into(),
                Some(mcpmesh_net::EndpointId::from_bytes(tls_id).principal()),
            );
            let sas = short_auth_code(&invite.inviter_id, &tls_id, &hello.secret);
            tracing::info!(code = %sas, "enrolled another device of this person (#86)");
            // Recorded on the ceremony surface like any pairing, so the inviter's human can read
            // the SAS off `status` and compare it — the check that makes this safe. Marked as an
            // ENROLLMENT structurally (#214): the row has no peer behind it, and a mismatch here
            // means an impostor now presents this identity, so the inviter's UI must route it to
            // `device_revoke`, not the `peer_remove` an ordinary mismatch gets.
            (ctx.record_pairing)(mcpmesh_local_api::RecentPairing {
                peer_nickname: "(this person's device)".into(),
                sas_code: sas.clone(),
                paired_at_epoch: epoch_now(),
                self_enroll: true,
            });
            let _ = send_reply(
                &mut send,
                &PairReply::Ok {
                    inviter_id: invite.inviter_id,
                    inviter_nickname: invite.nickname.clone(),
                    user_pk: Some(self_binding.user_pk.clone()),
                    binding_sig: Some(sig),
                },
            )
            .await;
            Ok(())
        }
        Redeem::Ok(invite) => {
            // Resolve any EXISTING entry for the TLS-authenticated redeemer id FIRST — a same-id
            // re-pair, or the REVERSE pairing of an earlier redeem (we redeemed THEIR invite
            // once, so our entry for them carries a real dial directory). The merge rules below
            // preserve what that entry already knows instead of replace-clobbering it.
            //
            // Display-uniqueness guard — the AUTHORITATIVE re-run of the #87 pre-check above,
            // AFTER winning the burn: two racing redeemers claiming the same NEW nickname can
            // both pass the pre-check (neither stored yet), so the loser must be caught here,
            // post-write-ordering. Burning in that race is rare and acceptable. Same shared
            // helper as the pre-check so the two cannot drift; no seam exists to interleave a
            // store write between peek and burn in a test, so this arm is a stated gap pinned
            // only through the helper (see the spec).
            //
            // The redeemer's self-asserted nickname becomes its resolved DISPLAY identity (the
            // gate maps endpoint_id → nickname); grants are principal-keyed (#38), so no access
            // can derive from the name — but a duplicate display name would make this inviter's
            // own records/routing ambiguous, so a name held by a DIFFERENT store peer is
            // refused. For an EXISTING same-id entry the self-suggested name is DISCARDED
            // entirely (the stored nickname is preserved below) — same-id re-pairs keep passing.
            // #87: the alias, when the invite carries one, is the name that gets stored.
            let claimed = effective_redeemer_nickname(
                invite.peer_nickname.as_deref(),
                &hello.redeemer_nickname,
            )
            .to_string();
            let (existing, collides) =
                resolve_and_check_collision(&ctx.store, &claimed, tls_id).await?;
            if collides {
                // #87: whether the invite SURVIVED is now a fact about this invite, not a
                // constant. `try_redeem` returns the count AFTER decrementing, so uses remaining
                // means the redeemer can rename and retry with the very same line.
                //
                // Hardcoding `false` here shipped the #147 defect straight back: a multi-use
                // invite makes this race routine (two colleagues redeeming one link, the #87(a)
                // same-hostname case), and the loser was told "ask the inviter for a fresh invite"
                // while holding one with four uses left — AND lost the branchable
                // ERR_NICKNAME_TAKEN precisely where the recovery is self-service.
                let survived = invite.uses_remaining > 0;
                tracing::warn!(
                    nickname = %claimed,
                    aliased = invite.peer_nickname.is_some(),
                    uses_remaining = invite.uses_remaining,
                    "pairing refused: nickname collision (post-redeem race guard)"
                );
                let _ = send_reply(
                    &mut send,
                    &collision_reply(invite.peer_nickname.as_deref(), &hello, survived),
                )
                .await;
                return Ok(());
            }

            // (1) TRUST/identity grant: record who this peer is so the AllowlistGate RESOLVES
            // its later mesh dial to this nickname. `endpoint_id` is the TLS-authenticated id.
            //
            // For a NEW peer: the redeemer's suggested nickname, `services = []` — the INVITER's
            // dial-back entry carries NO service grants (the asymmetric grant);
            // `PeerEntry.services` is a dial-directory, never an admission input, so this is the
            // correct encoding, not a functional lever. (Authorization is fact (2) below.)
            //
            // For an EXISTING same-id entry, MERGE — a second pairing must not clobber it:
            //  - nickname: PRESERVE the stored name. The inviter's chosen name for a peer is never
            //    renamed by the OTHER side's self-suggestion (a rename is the inviter's own act —
            //    `peer_rename` / re-REDEEMING a fresh invite on the naming side).
            //  - services: PRESERVE the dial directory. If we previously REDEEMED an invite from
            //    this peer, `services` records what WE may dial on THEM; the fresh `[]` applies
            //    only to a brand-new entry and must not wipe that directory (the reverse-pairing
            //    clobber bug).
            //  - user_id: a newly VERIFIED binding wins; otherwise keep the existing proven id —
            //    a verified user_id is never downgraded to `None` by a binding-less re-pair.
            //  - paired_at: keep the ORIGINAL stamp — the entry records when trust with this peer
            //    was FIRST established on this side (the re-pair itself is auditable via the
            //    trust event); stamp `now` only when the entry never had one (`internal peer add`).
            // A same-id re-pair KEEPS its stored name (never renamed behind the operator's back);
            // a brand-new peer gets `claimed`, which is the alias when the invite carried one.
            let nickname = existing
                .as_ref()
                .map_or_else(|| claimed.clone(), |e| e.nickname.clone());
            // The redeemer's OBSERVED direct address(es), from the live connection — the
            // pairing-proven dial-back hint. Merge rule: a fresh observation REFRESHES the hint;
            // learning nothing (relay-only, empty snapshot, serialize failure) preserves the
            // stored one — never downgrade `Some` to `None`.
            // #203: through `observed_for`, which filters to IP paths — the THIRD writer of
            // `last_addr`, and the one the 0.52.2 audit missed. It mapped EVERY path's
            // `remote_addr()` with no `is_ip()` filter, and iroh keeps a relay path open beside a
            // direct one, so a WAN pairing persisted a RELAY URL as the dial-back hint. Via the
            // merge below a relay-only observation then REPLACED a proven direct hint with one
            // that can never punch — which `dial_hint::observed_for`'s own rustdoc records as a
            // measured bug it exists to prevent (#124), reproduced here in a second copy of the
            // same logic.
            let observed_addr = crate::daemon::dial_hint::observed_for(&conn);
            let last_addr = crate::daemon::dial_hint::merge_hint(
                observed_addr,
                existing.as_ref().and_then(|e| e.last_addr.clone()),
            );
            let entry = PeerEntry {
                endpoint_id: tls_id,
                nickname: nickname.clone(),
                services: existing
                    .as_ref()
                    .map(|e| e.services.clone())
                    .unwrap_or_default(),
                paired_at: existing
                    .as_ref()
                    .and_then(|e| e.paired_at.clone())
                    .or_else(|| Some(now.to_string())),
                // The redeemer's PROVEN self-sovereign user_id, verified against its TLS id —
                // falling back to the already-proven stored id, else `None` (no/invalid binding:
                // the peer is stored nickname-only).
                user_id: verified_user_id(&hello.user_pk, &hello.binding_sig, &tls_id)
                    .or_else(|| existing.and_then(|e| e.user_id)),
                last_addr,
            };
            // IDENTITY REVOCATION (#218) — refused BEFORE the row and BEFORE the grant, on the
            // `user_id` the row would actually carry (the merge above: freshly verified, else the
            // already-proven stored one — a binding-less re-pair by a revoked person's known device
            // is the same case). Attestation has asked this since #85 ask 3; ordinary redemption
            // did not, so `peer_revoke b64u:` was void the moment a fresh invite was minted: the
            // person's next device landed a row and a `services[].allow` grant that the gate then
            // refused — the caller told "paired", every session refused. Same store predicate
            // the gate uses on the way in (`PeerStore::admission`'s identity check).
            //
            // After the burn, like the post-redeem collision guard: the caller proved a live secret
            // AND a binding for this identity, so the coded truth discloses nothing it did not
            // hold, and there is no invite-preserving remedy on this side to protect. A revoked
            // person holding a live invite is the invite you want gone.
            if let Some(uid) = entry.user_id.clone() {
                let store = ctx.store.clone();
                let revoked = tokio::task::spawn_blocking(move || store.is_user_revoked(&uid))
                    .await
                    .context("join identity revocation check")?;
                if revoked {
                    tracing::warn!(
                        nickname = %nickname,
                        "pairing refused: the redeemer's identity is revoked (invite consumed)"
                    );
                    let _ = send_reply(
                        &mut send,
                        &PairReply::Refused {
                            reason: REASON_IDENTITY_REVOKED.into(),
                            code: Some(RefusalCode::IdentityRevoked),
                        },
                    )
                    .await;
                    return Ok(());
                }
            }
            // The redeemer's STABLE principal, captured BEFORE the entry moves into the store:
            // the verified `b64u:` user_id when a binding was presented (or already proven),
            // else the `eid:` device principal of the TLS-AUTHENTICATED endpoint (#38).
            let principal = entry
                .user_id
                .clone()
                .unwrap_or_else(|| mcpmesh_net::EndpointId::from_bytes(tls_id).principal());
            // redb writes block + fsync — run on a blocking thread (mirrors `daemon::add_peer`'s
            // spawn_blocking + `.context(...)` + double-`?` join). A store write failure returns
            // here → the connection drops with a bare close (no explicit Refused frame), which
            // the redeemer treats as a refusal — acceptable for a rare disk error; the write is
            // one atomic redb txn, so no half-grant results.
            let store2 = ctx.store.clone();
            tokio::task::spawn_blocking(move || store2.add(entry))
                .await
                .context("join pair store write")??;

            // (2) AUTHORIZATION grant (the load-bearing step): append the redeemer's STABLE
            // principal — computed above from the verified binding / authenticated TLS id,
            // NEVER the display nickname (#38: names are rewritable, so a rename or re-pair
            // must not be able to desync a grant) — to each granted service's config
            // `[services.<svc>].allow` and RELOAD, so `select_service` actually admits it.
            // Fail-closed: propagate a grant failure so the pair FAILS rather than silently
            // leaving the peer known-but-forbidden. The invite is already burned (try_redeem
            // removed it), so on failure the redeemer must re-mint — acceptable, and correct:
            // no half-authorized peer.
            (ctx.grant)(principal, nickname.clone(), invite.services.clone()).await?;

            // Audit + completion notice — AFTER the durable trust write AND the durable grant,
            // BEFORE the network reply: the SAS (order-independent over both ids + the secret;
            // display-only, a pairing artifact not a surface leak) and the "paired" trust event.
            // Ordering it ahead of the reply means a committed pairing can never exist
            // un-audited (a reply-write failure must not swallow the notice).
            let sas = short_auth_code(&invite.inviter_id, &tls_id, &hello.secret);
            tracing::info!(peer = %nickname, code = %sas, "paired");
            // Park the SAS in the daemon's in-memory recent-pairings ring so the INVITER's human
            // can read it via `mcpmesh status` and compare it with the redeemer's (who got the
            // same words in its PairResult). Display-only ceremony state, lost on restart by
            // design; NOT trust data.
            (ctx.record_pairing)(mcpmesh_local_api::RecentPairing {
                peer_nickname: nickname,
                sas_code: sas,
                paired_at_epoch: now,
                self_enroll: false,
            });

            // The pairing is now durable + authorized + audited, so the reply is best-effort:
            // reply with OUR identity (both fields from the redeemed invite — no extra daemon
            // state) PLUS our self-sovereign device->user binding, if this daemon has a user key,
            // so the redeemer can store our user_id symmetrically (verified against our TLS id).
            // A failed write leaves the redeemer to re-check via a dial-back / the human noticing
            // the "paired" notice.
            let (inviter_pk, inviter_sig) = match ctx.self_binding {
                Some(b) => (Some(b.user_pk), Some(b.sig)),
                None => (None, None),
            };
            let _ = send_reply(
                &mut send,
                &PairReply::Ok {
                    inviter_id: invite.inviter_id,
                    inviter_nickname: invite.nickname.clone(),
                    user_pk: inviter_pk,
                    binding_sig: inviter_sig,
                },
            )
            .await;
            Ok(())
        }
        // Expired / Unknown: refuse with a GENERIC reason (no redemption oracle — do not leak
        // which). The specific variant is logged server-side only (no peer id, no secret). No
        // PeerEntry is written; an unknown secret did not burn a live invite.
        other => {
            tracing::info!(outcome = ?other, "pair attempt refused");
            let _ = send_reply(
                &mut send,
                &PairReply::Refused {
                    reason: REASON_REFUSED.into(),
                    // No code, on purpose (#147): this reason withholds
                    // unknown-vs-expired-vs-wrong-secret so it is not a redemption oracle, and a
                    // code labelling it would rebuild exactly that oracle.
                    code: None,
                },
            )
            .await;
            Ok(())
        }
    }
}

/// Best-effort refusal: log the attempt, send the refusal (ignoring any write error —
/// the redeemer treats a bare close as a refusal too), and return `Ok`.
async fn refuse(
    send: &mut iroh::endpoint::SendStream,
    reason: &str,
    log: &str,
) -> anyhow::Result<()> {
    tracing::info!("pair attempt refused: {log}");
    let _ = send_reply(
        send,
        &PairReply::Refused {
            reason: reason.into(),
            // The malformed-frame / id-mismatch refusals. Neither is a secret oracle, but neither
            // has a self-service remedy an embedder would write copy for, so neither is coded.
            code: None,
        },
    )
    .await;
    Ok(())
}

/// Write one reply frame and ensure it reaches the peer BEFORE the connection drops.
/// `write_frame` flushes into the QUIC send buffer; `finish()` signals stream end; `stopped()`
/// then resolves once the peer has ACKed receipt of every byte (noq: `Ok(None)`). Without the
/// `stopped()` wait, dropping `conn` at handler return could preempt the un-acked reply and the
/// redeemer would observe a bare close instead of the reply. `finish`/`stopped` are best-effort
/// (a vanished peer is not our problem); the meaningful error is the `write_frame` itself.
async fn send_reply(
    send: &mut iroh::endpoint::SendStream,
    reply: &PairReply,
) -> anyhow::Result<()> {
    write_frame(send, &serde_json::to_value(reply)?).await?;
    let _ = send.finish();
    let _ = send.stopped().await;
    Ok(())
}

/// Redeemer-side dial (`mcpmesh pair <invite>`): decode the invite, dial the inviter it
/// names on `mcpmesh/pair/1`, VERIFY the TLS-authenticated peer id binds the invite's `inviter_id`
/// (the address-swap defense) BEFORE revealing the secret, prove the secret, and — on the
/// inviter's `Ok` — write OUR dial-back [`PeerEntry`] and return the inviter's nickname + the SAS.
///
/// Asymmetric grant: OUR entry for the inviter carries `services = invite.services` — the
/// services we were granted and may DIAL on it (a client-side directory). The inviter's entry for
/// US carries no service grants (written on its side). The authorization that actually admits us
/// to those services is the inviter appending our nickname to its config `allow` — done in ITS
/// [`handle_inviter_side`] via [`grant_service_access`], not here.
///
/// Fail-closed: the identity check happens BEFORE `open_bi`/sending the secret, so a redeemer
/// that reaches a swapped address never reveals the bearer credential to the wrong peer.
///
/// [`grant_service_access`]: crate::daemon::grant_service_access
// Nine collaborators, each a distinct capability the redeemer needs (endpoint, our name, the line,
// our alias for them, whether we offered self-enrollment, the adopt hook, the store, our binding,
// the grant-back hook). A params struct
// would rename them without reducing them, and the signature is pinned by the integration tests.
#[allow(clippy::too_many_arguments)]
pub async fn redeem_invite(
    endpoint: iroh::Endpoint,
    self_nickname: String,
    invite_line: String,
    // #87: OUR local name for the inviter, overriding the one its invite suggests. Local only —
    // never sent, and it does not bypass the squat check below.
    as_nickname: Option<String>,
    // #178: which ceremony this caller is willing to complete. REQUIRED — see [`SelfEnroll`] for
    // why it is not defaulted.
    self_enroll: SelfEnroll,
    // #86: install an adopted self-enrollment binding. `None` = a caller that cannot persist one
    // (a fixture), in which case a self-enrollment still verifies but is not retained.
    adopt_binding: Option<AdoptBindingFn>,
    store: Arc<PeerStore>,
    self_binding: Option<SelfBinding>,
    grant_back: Option<GrantBackFn>,
) -> anyhow::Result<PairResult> {
    let invite = Invite::decode(&invite_line)?;
    // #178: refuse a ceremony the caller never offered, HERE — before the nickname check, before
    // the dial, before the secret is on the wire. The outcome of a self-enrollment is a device→user
    // binding admitting this device to everyone who trusts the inviter's `user_id`, and #86 gives
    // no revocation short of rotating that user key. So it cannot be a thing a caller observes
    // afterwards on `enrolled_as_self` and declines: by then it is done.
    //
    // Refusing before the dial is what keeps the invite USABLE. Nothing is contacted and nothing is
    // burned, so the identical line still works the moment the person is offered the real choice —
    // a refusal here costs a round trip, not the credential.
    if invite.as_self && self_enroll == SelfEnroll::Refuse {
        bail!(PairRefusal::new(
            mcpmesh_local_api::ERR_SELF_ENROLL_NOT_OFFERED,
            "this is a device-enrollment link, not a pairing invite: redeeming it would make this \
             device another device of that person's identity, which this application did not offer \
             — retry with allow_self_enroll if that is what you meant",
        ));
    }
    // ONE binding for the name we will use everywhere below — the squat check, the stored entry,
    // the grant-back display name, and the result. Computing it per-site is how a check ends up
    // running against a different name than the one written.
    let as_nickname_used = as_nickname.is_some();
    let local_name = as_nickname.unwrap_or_else(|| invite.nickname.clone());

    // Client-side pre-check: a friendly early error for an expired invite (the inviter also
    // enforces at redeem — this just avoids a pointless dial).
    if invite.expires_at_epoch < epoch_now() {
        // #159: decided from the line in hand, before any dial — so it reveals nothing about the
        // inviter and is safe to name precisely.
        bail!(PairRefusal::new(
            mcpmesh_local_api::ERR_INVITE_EXPIRED,
            "invite expired",
        ));
    }

    // Client-side nickname-squatting check — the mirror of the inviter side's
    // [`nickname_collision`], and enforced BEFORE the dial so a squatting invite never reaches
    // the wire. `invite.nickname` is a stranger's SUGGESTION for what we should call them, and
    // applying it verbatim is what our gate resolves the inviter's DISPLAY name to (and what
    // our own outbound `<peer>/<service>` routing keys on — first-match by name). Grants are
    // principal-keyed (#38), so no access can follow the name; refusing here keeps the
    // invariant that redeeming an invite grants the other side nothing.
    if !invite.as_self
        && let Some(conflict) = nickname_squat(&store, &local_name, &invite.inviter_id)?
    {
        // The wording has to follow WHOSE name collided (#87 gate). With `as_nickname` set, the
        // invite asked for nothing of the sort — the local user picked it — and "ask them for a
        // different name" is advice for a problem they do not have.
        let reason = if as_nickname_used {
            format!(
                "you asked to call this peer '{local_name}', but {conflict} \
                 Redeem the same invite again with a different name."
            )
        } else {
            // BYTE-IDENTICAL to before this change. A downstream forwards this prose to end
            // users, and #159 shipped `-32048` precisely so the code — not a reword — is how a
            // consumer offers the better remedy. Only the NEW aliased case above gets new wording,
            // because it has no existing consumer.
            format!(
                "this invite asks to be called '{local_name}', but {conflict} \
                 Ask them for an invite suggesting a different name."
            )
        };
        bail!(PairRefusal::new(
            mcpmesh_local_api::ERR_INVITE_NAME_CONFLICT,
            reason,
        ));
    }

    // REVOCATION, REDEEMER SIDE (#218 review) — before the dial, before the secret is on the wire.
    // An ordinary redemption writes a row for the inviter AND runs the #43 grant-back, appending
    // its principal to every service this node serves; with the inviter revoked here, the row is
    // one our own gate refuses, `pair` reported success, and a later `peer_unrevoke` would silently
    // switch on grants this operator never deliberately made. `is_refused` covers what is knowable
    // from the line: the inviter's endpoint, and the identity its existing row carries. Nothing is
    // contacted, so the invite is untouched. A self-enrollment writes neither row nor grant.
    //
    // The code is `-32056` rather than a pairing code: its remedy ("`peer_unrevoke` here if the
    // revocation was a mistake") is true for THIS node's operator, while `-32045` says the invite
    // is dead and `-32057` says the OTHER side revoked you — both false here.
    if !invite.as_self {
        let (s, id) = (store.clone(), invite.inviter_id);
        let refused = tokio::task::spawn_blocking(move || s.is_refused(&id))
            .await
            .context("join inviter revocation check")?;
        if refused {
            bail!(PairRefusal::new(
                mcpmesh_local_api::ERR_PRINCIPAL_REVOKED,
                "this inviter is revoked on this node, so pairing with it would write a row and \
                 grants your own gate refuses — nothing was contacted and the invite is untouched. \
                 Lift the revocation first (peer_unrevoke) if it was a mistake",
            ));
        }
    }

    // Dial the inviter at the exact address the invite embeds — pairing needs no discovery
    // (the invite carries the dialable `EndpointAddr`, so this works on localhost too).
    let addr: iroh::EndpointAddr = serde_json::from_str(&invite.inviter_addr_json)
        .context("invite carries an undecodable inviter address")?;
    // #203: the invite's addresses are a REMOTE party's claim, and this dials them before anything
    // is stored — so the stored-hint filter never sees them. Without this a crafted invite aims
    // this node's QUIC Initials (padded to >=1200 bytes) at multicast, broadcast or `0.0.0.0`,
    // which on Linux is localhost. The identity check below is unaffected: TLS still authenticates
    // whoever answers, and this only removes destinations that cannot be a peer at all.
    let addr = crate::daemon::dial::dialable_only(addr);
    // #223 review: the revocation check above is on `inviter_id`, but the dial goes to the id INSIDE
    // the address. A line whose two disagree would pass that check for a live id and dial a revoked
    // one — and the post-handshake address-swap defense below runs only after the handshake has
    // already told that endpoint who we are. Refused before any packet, with the same code.
    if *addr.id.as_bytes() != invite.inviter_id {
        bail!(PairRefusal::new(
            mcpmesh_local_api::ERR_INVITER_MISMATCH,
            "inviter id mismatch — refusing (address-swap defense)",
        ));
    }
    // #159: unreachable is its own condition — the invite is untouched, so the remedy is "check
    // they are running and retry the same line", not "get a new one".
    let conn = endpoint
        .connect(addr, mcpmesh_net::ALPN_PAIR)
        .await
        // #159: the CODE is added; the message and its cause chain are untouched. The porcelain
        // owns the human explanation for this one (`render.rs` turns a self-redeem into "you
        // cannot redeem your own invite on the machine that minted it"), it MATCHES ON THIS
        // STRING, and it needs iroh's cause underneath. Rewording it here made that branch dead
        // code and replaced a correct explanation with advice — "retry this same invite" — that
        // can never work for the most common newcomer mistake (#159 gate).
        .map_err(|e| {
            // `.context(PairRefusal)` rather than `Error::new(PairRefusal).context(e)`: context
            // goes on TOP, so this keeps our message as the Display (the porcelain matches on it)
            // AND keeps iroh's error as the source, so `{:#}` still carries the cause for
            // diagnostics. Wrapping the other way round hid our message behind iroh's.
            anyhow::Error::new(e).context(PairRefusal::new(
                mcpmesh_local_api::ERR_INVITER_UNREACHABLE,
                "could not dial the inviter's machine",
            ))
        })?;

    // Address-swap defense: the TLS-authenticated peer id is AUTHORITATIVE. If it is not the
    // id the invite names, we reached a substituted/MITM endpoint — refuse BEFORE revealing the
    // secret. (A whole-invite forgery that also swapped `inviter_id` still diverges the SAS,
    // which the human catches out-of-band.)
    if *conn.remote_id().as_bytes() != invite.inviter_id {
        // #159: the ONE onboarding refusal that must not be rendered as "try again". Something
        // answered in place of the machine this invite names.
        bail!(PairRefusal::new(
            mcpmesh_local_api::ERR_INVITER_MISMATCH,
            "inviter id mismatch — refusing (address-swap defense)",
        ));
    }

    // We (the redeemer) OPEN the bi-stream; the inviter `accept_bi`s. Send the hello proving the
    // secret. `redeemer_id` is our own TLS id (the inviter re-verifies it against remote_id).
    //
    // The whole open→write→read exchange is ONE async block so its failure is classified against
    // `close_reason()` at ONE site (the #89 `exchange()` shape): the accept gate's fast-close
    // races all three stream calls, and on a real link the close can land before `open_bi` or
    // the hello write completes — a first version guarded only the read arm, which the second
    // #142-style gate caught as an intermittent recurrence of the bare-connection-failure UX
    // this exists to remove. On localhost the read always loses the race, so only the
    // single-site SHAPE guarantees the other two; the dead-invite test pins this site.
    let (redeemer_pk, redeemer_sig) = match self_binding {
        Some(b) => (Some(b.user_pk), Some(b.sig)),
        None => (None, None),
    };
    let hello = RedeemerHello {
        secret: invite.secret,
        redeemer_id: *endpoint.id().as_bytes(),
        redeemer_nickname: self_nickname,
        user_pk: redeemer_pk,
        binding_sig: redeemer_sig,
        attest: false,
    };
    let exchange = async {
        let (mut send, recv) = conn.open_bi().await.context("open the pairing bi-stream")?;
        write_frame(&mut send, &serde_json::to_value(&hello)?)
            .await
            .context("send the pairing hello")?;
        // Read exactly ONE reply frame (same cap as the inviter side).
        let mut reader = FrameReader::new(BufReader::new(recv), MAX_PAIR_FRAME);
        match reader.next().await.context("read the pairing reply")? {
            Some(Inbound::Frame(v)) => {
                serde_json::from_value::<PairReply>(v).context("inviter reply is not a PairReply")
            }
            _ => bail!("no reply from the inviter (connection closed before a reply)"),
        }
    };
    let reply: PairReply = match exchange.await {
        Ok(reply) => reply,
        // The exchange failed. If the inviter's accept gate fast-closed us (#87b), say what
        // that MEANS — the invite line in hand may still advertise a live TTL, but invites are
        // in-memory on the inviter, so this is the everyday shape of "expired, already used,
        // or the inviter cancelled it", not a network problem.
        //
        // It used to say "the inviter's daemon restarted (invites do not survive a restart)".
        // #87b made them survive, so that sentence became a false explanation handed to a user in
        // the one place they are trying to work out what went wrong.
        Err(_) if no_live_invite_close(&conn) => {
            bail!(PairRefusal::new(
                mcpmesh_local_api::ERR_INVITE_NOT_LIVE,
                "the invite is no longer live on the inviter: it expired, was already \
                 redeemed, or the inviter cancelled it — ask for a fresh invite",
            ));
        }
        Err(e) => return Err(e),
    };
    // On Ok, verify the inviter's presented binding against `invite.inviter_id` (which we proved
    // equals the TLS-authenticated id above) → its PROVEN user_id, or `None` if it presented none.
    let inviter_user_id = match &reply {
        PairReply::Refused { reason, code } => return Err(refusal_error(reason, *code)),
        PairReply::Ok {
            user_pk,
            binding_sig,
            ..
        } => verified_user_id(user_pk, binding_sig, &invite.inviter_id),
    };
    // #86 SELF-ENROLLMENT: this was not a pairing at all — we are another DEVICE of the inviter's
    // person. Adopt the binding they signed for OUR endpoint and write NO peer row: a row would put
    // this person in their own contact list and make their own other device an authorizable
    // principal here.
    //
    // The binding is verified against OUR endpoint (`self_id`), not the inviter's — it is ours to
    // present from now on, and one signed for anyone else would be useless to us.
    if invite.as_self {
        let self_id = *endpoint.id().as_bytes();
        let PairReply::Ok {
            user_pk,
            binding_sig,
            ..
        } = &reply
        else {
            unreachable!("the Refused arm returned above")
        };
        let (Some(user_pk), Some(sig)) = (user_pk, binding_sig) else {
            bail!(PairRefusal::new(
                mcpmesh_local_api::ERR_INVITE_REFUSED,
                "the inviter completed a self-enrollment without issuing a binding",
            ));
        };
        mcpmesh_trust::binding::verify_presented(user_pk, sig, &self_id).map_err(|_| {
            // Coded like every sibling refusal on this path (#159): a consumer branches on the
            // code rather than matching prose. The inner error is a roster-layer string and says
            // nothing useful to a redeemer, so it is not interpolated.
            anyhow::Error::from(PairRefusal::new(
                mcpmesh_local_api::ERR_INVITE_REFUSED,
                "the enrollment binding does not verify for this device",
            ))
        })?;
        if let Some(adopt) = adopt_binding {
            adopt(SelfBinding {
                user_pk: user_pk.clone(),
                sig: sig.clone(),
            })
            .await?;
        }
        let sas_code = short_auth_code(&invite.inviter_id, &self_id, &invite.secret);
        return Ok(PairResult {
            peer_nickname: local_name,
            sas_code,
            enrolled_as_self: true,
            services: vec![],
            app_label: invite.app_label,
            peer_user_id: Some(user_pk.clone()),
        });
    }

    // …and the identity the inviter PROVED in its reply (#218 review), which the line does not
    // carry — so this one runs after the exchange: the invite is consumed, and the inviter may have
    // recorded the pairing on its side. Nothing is written HERE: no row, no grant-back.
    if let Some(uid) = inviter_user_id.clone() {
        let s = store.clone();
        let uid_r = uid.clone();
        let revoked = tokio::task::spawn_blocking(move || s.is_user_revoked(&uid_r))
            .await
            .context("join inviter identity revocation check")?;
        if revoked {
            bail!(PairRefusal::new(
                mcpmesh_local_api::ERR_PRINCIPAL_REVOKED,
                format!(
                    "the inviter proved identity '{uid}', which is revoked on this node — no row \
                     and no grant were written here (the invite is consumed, and the inviter may \
                     have recorded the pairing on its side). Lift the revocation first \
                     (peer_unrevoke) if it was a mistake"
                ),
            ));
        }
    }

    // Returned to the redeemer in PairResult (#30) so it learns the peer's STABLE identity at
    // pair time — cloned before `inviter_user_id` is moved into the stored PeerEntry below.
    let peer_user_id = inviter_user_id.clone();

    // Our dial-back entry: the inviter, named by the invite's suggested nickname, granting the
    // services WE may dial on it (the asymmetric grant) — MERGED with any existing entry for this
    // inviter (a repeat grant: Alice grants notes, later invites again granting kb):
    //  - services: UNION(existing, invite.services) — the client-side dial directory ACCUMULATES
    //    grants (dedup; stable order: existing entries first, new grants appended);
    //  - nickname: the NEW invite's suggested nickname — renaming a peer by redeeming a fresh
    //    invite is a deliberate feature (no unpair needed), so the new suggestion wins here;
    //  - user_id: the newly VERIFIED binding wins, else keep the existing proven id — a verified
    //    user_id is never downgraded to `None` by a binding-less re-pair;
    //  - paired_at: now — this side stamps each redeem (each is a fresh ceremony we performed);
    //  - last_addr: the address THIS CONNECTION actually used, read off the live connection —
    //    never the invite's address list (#203).
    //
    //    The old comment called the invite's list "the pairing-PROVEN dialable address (we just
    //    reached the inviter through it)". That is true of AT MOST ONE of the addresses in it, and
    //    the rest were a remote party's unvalidated claim being persisted as dial targets — which
    //    every later dial then sprays QUIC Initials at, because iroh sends to every known path
    //    until one is selected. `observed_for` returns exactly the one(s) we reached, filtered to
    //    IP, which is the same rule `dial_hint::refresh` has always used for a live session.
    //
    //    Relay-only pairing yields `None`, which leaves any existing hint alone rather than
    //    clearing it — the same "learned nothing" rule. That pairing reached the inviter through a
    //    relay, so a relay/discovery dial is what it will keep using.
    // `endpoint_id` is `invite.inviter_id`, which we verified above equals the TLS id.
    // Resolve + merge + add run in ONE blocking closure (redb reads/writes block + fsync).
    let inviter_id = invite.inviter_id;
    let nickname = local_name.clone();
    let granted = invite.services.clone();
    let paired_at = Some(epoch_now().to_string());
    let last_addr = crate::daemon::dial_hint::observed_for(&conn);
    tokio::task::spawn_blocking(move || {
        let existing = store.resolve(&inviter_id)?;
        let mut services = existing
            .as_ref()
            .map(|e| e.services.clone())
            .unwrap_or_default();
        for svc in granted {
            if !services.contains(&svc) {
                services.push(svc);
            }
        }
        store.add(PeerEntry {
            endpoint_id: inviter_id,
            nickname,
            services,
            paired_at,
            user_id: inviter_user_id.or_else(|| existing.as_ref().and_then(|e| e.user_id.clone())),
            // NEVER downgrade a known hint to `None` (#203 gate). `store.add` is a replace-on-id
            // upsert, and until 0.52.2 this value was ALWAYS `Some` (the invite's list), so the
            // field needed no merge guard and had none. Switching it to `observed_for` — which
            // returns `None` for a relay-only connection — made the missing guard load-bearing
            // overnight: a re-redeem that happened to complete over the relay WIPED a proven
            // direct hint. Every sibling field kept its guard; this was the only one without one,
            // while five places (including this function's own new comment) asserted "leave alone,
            // never clear".
            last_addr: crate::daemon::dial_hint::merge_hint(
                last_addr,
                existing.and_then(|e| e.last_addr),
            ),
        })
    })
    .await
    .context("join redeemer store write")??;

    // #43: MUTUAL grant. The inviter granted us its services on its side (its `GrantFn`);
    // symmetrically we now grant the INVITER access to ALL services WE serve, under the SAME
    // stable-principal rule (its verified `b64u:` when it presented a binding, else its
    // `eid:`). One ceremony ⇒ both directions admitted; the SAS already covered both humans.
    // The daemon supplies the hook; tests pass `None` (they assert the store write only).
    if let Some(grant_back) = grant_back {
        let inviter_principal = peer_user_id
            .clone()
            .unwrap_or_else(|| mcpmesh_net::EndpointId::from_bytes(invite.inviter_id).principal());
        // `local_name`, not `invite.nickname`: the grant-back display name is OURS.
        grant_back(inviter_principal, local_name.clone()).await?;
    }

    // Display-only SAS, order-independent → equals the inviter's. Both humans read it
    // aloud to catch a whole-invite forgery out-of-band.
    let self_id = *endpoint.id().as_bytes();
    let sas_code = short_auth_code(&invite.inviter_id, &self_id, &invite.secret);
    Ok(PairResult {
        peer_nickname: local_name,
        sas_code,
        // The services WE were granted (from the invite) — the porcelain renders each as
        // `<peer>/<service>` for the "You can mount:" line. Same list written into our
        // dial-back `PeerEntry.services` above (a client-side dial directory).
        services: invite.services,
        // The opaque app label the inviter attached (#31), echoed to the embedder verbatim.
        // mcpmesh never interpreted it — it is display/metadata only.
        app_label: invite.app_label,
        // The inviter's proven stable user_id (#30) — the redeemer's portable handle for it, and
        // what it may pass to open_session to dial by identity rather than nickname.
        peer_user_id,
        enrolled_as_self: false,
    })
}

/// Turn an inviter's refusal into the redeemer's error (#147).
///
/// A CODED refusal becomes a typed [`NicknameTaken`], which `respond` downcasts to
/// [`ERR_NICKNAME_TAKEN`](mcpmesh_local_api::ERR_NICKNAME_TAKEN) — so an embedder branches on a
/// number instead of substring-matching prose that is generated on the other side of the wire and
/// that it cannot rewrite. Everything else stays an opaque `-32000`, including a refusal from an
/// inviter older than 0.25.1 (which sends no code) and one whose kind this node predates.
///
/// The reason is carried VERBATIM rather than rebuilt: the inviter is the side that knows whether
/// the invite survived, so re-deriving the sentence here would be a second source of truth for it.
fn refusal_error(reason: &str, code: Option<RefusalCode>) -> anyhow::Error {
    let msg = format!("pairing refused: {reason}");
    match code {
        Some(RefusalCode::NicknameTaken) => anyhow::Error::new(NicknameTaken(msg)),
        // #218: the inviter revoked THIS identity. Its own code, because the remedy every other
        // refusal implies — a fresh invite — is refused the same way while the identity is presented.
        Some(RefusalCode::IdentityRevoked) => anyhow::Error::new(PairRefusal::new(
            mcpmesh_local_api::ERR_PAIR_IDENTITY_REVOKED,
            msg,
        )),
        // #159: branchable as "that invite did not work" WITHOUT saying why. The inviter answers
        // one reason for unknown / expired / wrong secret on purpose — telling them apart is a
        // redemption oracle — so this code carries exactly as much as the prose already did.
        _ => anyhow::Error::new(PairRefusal::new(mcpmesh_local_api::ERR_INVITE_REFUSED, msg)),
    }
}

/// Display-uniqueness guard for pairing. Returns `true` = REFUSE when a redeemer's
/// self-asserted `nickname` is already held by a DIFFERENT stored peer: a duplicate display
/// name would make the inviter's own records ambiguous (status shows two peers as one, and
/// outbound routing by name is first-match). NOT a privilege defense anymore (#38): grants
/// are principal-keyed, so no name can inherit or confer access — this protects display and
/// routing clarity only.
///
/// A same-id re-pair (every same-name entry shares `tls_id`) passes: that peer's own name is
/// no duplicate. Blocking (redb read) — call on a blocking thread.
fn nickname_collision(
    store: &PeerStore,
    nickname: &str,
    tls_id: &[u8; 32],
) -> anyhow::Result<bool> {
    Ok(store
        .list()?
        .into_iter()
        .any(|e| e.nickname == nickname && &e.endpoint_id != tls_id))
}

/// Redeemer-side name-squatting guard — the mirror of [`nickname_collision`], run before we
/// adopt an invite's *suggested* nickname. Returns `Some(reason)` = REFUSE when a stored peer
/// already holds this nickname under a DIFFERENT `endpoint_id`: adopting it would make OUR
/// outbound `<peer>/<service>` routing ambiguous (first-match by name) and our records show
/// two peers as one. NOT an access defense anymore (#38): grants are principal-keyed, so a
/// name confers nothing — this protects the redeemer's own routing/display clarity.
///
/// Re-pairing with the SAME endpoint passes, so rename-by-a-fresh-invite keeps working — and
/// post-#38 that rename is fully SAFE: no grant keys on the name it rewrites.
///
/// The returned string is a reason phrase, spliced into the caller's guidance message.
fn nickname_squat(
    store: &PeerStore,
    nickname: &str,
    inviter_id: &[u8; 32],
) -> anyhow::Result<Option<String>> {
    let clashes = store
        .list()?
        .into_iter()
        .any(|e| e.nickname == nickname && &e.endpoint_id != inviter_id);
    Ok(clashes.then(|| {
        "you already use that name for a different peer — \
         accepting it would make your own dials to that name ambiguous. \
         Unpair the existing peer first if you no longer need it."
            .to_string()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #147: the refusal states the ACTION, not a control verb.
    ///
    /// `set_nickname` is control-API vocabulary. A GUI user cannot type it, and the embedder that
    /// DISPLAYS this string is not the one that could rewrite it — the message is built on the
    /// inviter and travels to the redeemer, so a downstream fix means substring-matching our prose.
    /// The burned-invite sibling was always the model and is asserted here so it stays that way.
    #[test]
    fn the_refusal_names_an_action_not_a_control_verb() {
        let survived = reason_nickname_taken("studio-mac", true);
        assert!(
            !survived.contains("set_nickname"),
            "no control verb may appear in a string a human is shown: {survived}"
        );
        assert!(survived.contains("rename this node"), "got {survived}");
        assert!(
            survived.contains("the invite was NOT consumed"),
            "the recoverable case must still say the invite survived — that is what makes the \
             advice actionable (#87): {survived}"
        );
        assert!(survived.contains("studio-mac"), "got {survived}");

        let burned = reason_nickname_taken("studio-mac", false);
        assert!(
            burned.contains("ask the inviter for a fresh invite"),
            "the burned-invite clause names an action already; it must not regress: {burned}"
        );
        assert!(!burned.contains("set_nickname"), "got {burned}");
    }

    /// #147: `code` is additive both ways — absent on an older inviter's reply, and unrecognized
    /// from a newer one. Either must degrade rather than fail the whole reply, or an informative
    /// refusal becomes an opaque parse error on a pinned redeemer.
    #[test]
    fn a_refusal_code_is_additive_and_degrades() {
        // An inviter older than 0.25.1: no `code` key at all.
        let old: PairReply = serde_json::from_value(
            serde_json::json!({"result": "refused", "reason": "pairing refused"}),
        )
        .expect("an older inviter's refusal must still parse");
        let PairReply::Refused { code, reason } = old else {
            panic!("expected a refusal");
        };
        assert_eq!(code, None, "absent means absent — never a guessed kind");
        assert_eq!(reason, "pairing refused");

        // A refusal kind from a NEWER inviter, and every non-string shape a proxy might produce.
        for bad in [
            serde_json::json!("invite_expired"),
            serde_json::Value::Null,
            serde_json::json!(7),
            serde_json::json!(true),
            serde_json::json!({"kind": "nickname_taken", "nested": [1, 2]}),
            serde_json::json!(["nickname_taken"]),
        ] {
            let v = serde_json::json!({"result": "refused", "reason": "r", "code": bad});
            let reply: PairReply = serde_json::from_value(v)
                .unwrap_or_else(|e| panic!("`code: {bad}` must not fail the whole reply: {e}"));
            let PairReply::Refused { code, reason } = reply else {
                panic!("expected a refusal");
            };
            assert_eq!(reason, "r", "the rest of the reply survives: {bad}");
            // `null` is an ABSENT code, not an unknown one — `Option` absorbs it first.
            assert!(
                matches!(code, Some(RefusalCode::Unknown) | None),
                "an unreadable code must degrade, not claim a kind: {bad} -> {code:?}"
            );
        }
    }

    /// #218: the identity-revocation refusal is coded on the pairing wire (`identity_revoked`),
    /// survives the round trip, and the REDEEMER maps it to its OWN control code — never to
    /// `-32049`, whose documented remedy ("ask for a fresh invite") is the one thing that does not
    /// help. The real send site is pinned end to end by
    /// `a_revoked_identity_cannot_redeem_until_the_inviter_unrevokes_it` in
    /// `cli/tests/pairing_rendezvous.rs`; this pins the two mappings that test rides on.
    ///
    /// Deleting the `IdentityRevoked` arm in `refusal_error` fails the `code()` assertion
    /// (it falls through to `-32049`); deleting the `"identity_revoked"` arm in the deserializer
    /// fails the round trip (`Unknown`).
    #[test]
    fn the_identity_revoked_refusal_is_coded_and_maps_to_its_own_control_code() {
        let wire = serde_json::to_value(PairReply::Refused {
            reason: REASON_IDENTITY_REVOKED.into(),
            code: Some(RefusalCode::IdentityRevoked),
        })
        .unwrap();
        assert_eq!(wire["code"], "identity_revoked", "got {wire}");

        let reply: PairReply = serde_json::from_value(wire).unwrap();
        let PairReply::Refused { code, reason } = reply else {
            panic!("expected a refusal");
        };
        assert_eq!(code, Some(RefusalCode::IdentityRevoked));
        let e = refusal_error(&reason, code);
        let refusal = e
            .downcast_ref::<PairRefusal>()
            .unwrap_or_else(|| panic!("must be the coded PairRefusal, got: {e:#}"));
        assert_eq!(
            refusal.code(),
            mcpmesh_local_api::ERR_PAIR_IDENTITY_REVOKED,
            "its own code, not the opaque -32049"
        );
        let msg = e.to_string();
        assert!(
            msg.contains("refuses any device presenting it") && msg.contains("peer_unrevoke"),
            "the message says what is refused and who lifts it: {e:#}"
        );
        assert!(
            !msg.contains("will not help") && !msg.contains("fresh invite"),
            "and does not promise another invite fails — a person who stops presenting the \
             identity is not refused by it: {e:#}"
        );

        // The neighbours are untouched: a kind this node predates, and no code at all, still
        // degrade to the opaque refusal.
        for code in [Some(RefusalCode::Unknown), None] {
            let e = refusal_error("r", code);
            assert_eq!(
                e.downcast_ref::<PairRefusal>().expect("PairRefusal").code(),
                mcpmesh_local_api::ERR_INVITE_REFUSED,
                "{code:?}"
            );
        }
    }

    /// #147: the serialized wire SHAPE of a coded and an uncoded refusal — the `snake_case`
    /// rendering and `skip_serializing_if` eliding the key rather than sending `null`.
    ///
    /// Scope note, because the first version of this test overreached: it builds its own
    /// `PairReply`, so it pins the SERIALIZER, not the branch that chooses a code. The
    /// oracle boundary — that an unproven caller's refusal carries none — is pinned on the real
    /// send site by `a_wrong_secret_with_a_colliding_nickname_gets_only_the_generic_refusal` in
    /// `cli/tests/pairing_rendezvous.rs`. A mutation stamping the code at that send site passed
    /// THIS test.
    #[test]
    fn the_opaque_refusal_carries_no_code() {
        let coded = serde_json::to_value(PairReply::Refused {
            reason: reason_nickname_taken("bob", true),
            code: Some(RefusalCode::NicknameTaken),
        })
        .unwrap();
        assert_eq!(coded["code"], "nickname_taken", "got {coded}");

        let generic = serde_json::to_value(PairReply::Refused {
            reason: REASON_REFUSED.into(),
            code: None,
        })
        .unwrap();
        assert!(
            generic.get("code").is_none(),
            "the opaque refusal must carry NO code — one would make it a redemption oracle: \
             {generic}"
        );
        assert_eq!(
            generic["reason"], REASON_REFUSED,
            "and its reason stays opaque: {generic}"
        );
    }

    /// #147: ONLY a coded collision refusal becomes the typed error `respond` maps to
    /// `ERR_NICKNAME_TAKEN`. This is the branch the whole issue turns on: if the generic refusal
    /// also downcast, an embedder branching on the code would tell a user "rename and retry" for a
    /// wrong-or-expired secret; if the coded one did NOT, the embedder is back to reading prose.
    ///
    /// The `None` case is an inviter older than 0.25.1 — it must land on the generic arm, not be
    /// guessed into a kind.
    #[test]
    fn only_a_coded_collision_refusal_becomes_the_typed_error() {
        let wire = reason_nickname_taken("studio-mac", true);
        let coded = refusal_error(&wire, Some(RefusalCode::NicknameTaken));
        assert!(
            coded.downcast_ref::<NicknameTaken>().is_some(),
            "respond's downcast arm is what maps this to ERR_NICKNAME_TAKEN: {coded}"
        );
        assert!(coded.to_string().contains("rename this node"), "{coded}");

        for opaque in [None, Some(RefusalCode::Unknown)] {
            let e = refusal_error(REASON_REFUSED, opaque);
            assert!(
                e.downcast_ref::<NicknameTaken>().is_none(),
                "an opaque refusal must NOT claim the collision code — an embedder would tell a                  user to rename after a wrong or expired secret: {opaque:?} -> {e}"
            );
            assert_eq!(e.to_string(), format!("pairing refused: {REASON_REFUSED}"));
        }
    }

    /// A minimal invite naming `inviter`, for the refusal-code call-site tests.
    fn sample_invite_for(inviter: iroh::EndpointId, expires_at_epoch: u64) -> Invite {
        Invite {
            secret: [5u8; 32],
            inviter_id: *inviter.as_bytes(),
            inviter_addr_json: serde_json::to_string(&iroh::EndpointAddr::from(inviter)).unwrap(),
            nickname: "alice".into(),
            services: vec!["notes".into()],
            expires_at_epoch,
            app_label: None,
            uses_remaining: 1,
            peer_nickname: None,
            as_self: false,
        }
    }

    /// #159 gate: each refusal carries its own code AT ITS CALL SITE.
    ///
    /// The first version tested this by handing `respond` a `PairRefusal::new(code, ..)` and
    /// checking it returned that number — i.e. that `respond` can read a field. Rewriting all six
    /// call sites to the SAME wrong code left the entire workspace green: five of the six were
    /// pinned by nothing, and could have been wired to each other's codes unnoticed.
    ///
    /// These drive the real functions. The two that need a live inviter (`ERR_INVITE_NOT_LIVE`,
    /// `ERR_INVITE_REFUSED`) are covered by the rendezvous integration suite; the rest are decided
    /// locally and are covered here.
    #[tokio::test]
    async fn each_refusal_call_site_uses_its_own_code() {
        use mcpmesh_local_api as api;
        let code_of = |e: &anyhow::Error| e.downcast_ref::<PairRefusal>().map(|r| r.code());

        let store = || {
            std::sync::Arc::new(
                crate::allowlist::PeerStore::open(
                    &tempfile::tempdir().unwrap().keep().join("s.redb"),
                )
                .unwrap(),
            )
        };
        let ep = || async {
            iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .bind()
                .await
                .unwrap()
        };
        let inviter = iroh::SecretKey::from_bytes(&[41u8; 32]).public();

        // EXPIRED — decided from the line in hand, before any dial.
        let mut expired = sample_invite_for(inviter, 1);
        expired.expires_at_epoch = 1;
        let e = redeem_invite(
            ep().await,
            "me".into(),
            expired.encode(),
            None,
            SelfEnroll::Refuse,
            None,
            store(),
            None,
            None,
        )
        .await
        .expect_err("an expired line must refuse");
        assert_eq!(
            code_of(&e),
            Some(api::ERR_INVITE_EXPIRED),
            "the local expiry pre-check owns ERR_INVITE_EXPIRED: {e:#}"
        );

        // NAME CONFLICT — the invite's suggested name is already ours for a different peer.
        let s = store();
        s.add(crate::allowlist::PeerEntry {
            endpoint_id: [0xAB; 32],
            nickname: "taken".into(),
            services: vec![],
            paired_at: None,
            user_id: None,
            last_addr: None,
        })
        .unwrap();
        let mut named = sample_invite_for(inviter, 9_999_999_999);
        named.nickname = "taken".into();
        let e = redeem_invite(
            ep().await,
            "me".into(),
            named.encode(),
            None,
            SelfEnroll::Refuse,
            None,
            s.clone(),
            None,
            None,
        )
        .await
        .expect_err("a squatting invite must refuse");
        assert_eq!(
            code_of(&e),
            Some(api::ERR_INVITE_NAME_CONFLICT),
            "the redeemer-side squat check owns ERR_INVITE_NAME_CONFLICT: {e:#}"
        );

        // #87: the SAME squatting invite succeeds past the name check when the redeemer supplies
        // its own local alias. This is the whole point — before it, the only fixes were to ask the
        // inviter to re-mint or to unpair whoever holds the name. It still fails afterwards (the
        // sample invite is undialable), but with a DIFFERENT code: the name check is behind it.
        let e = redeem_invite(
            ep().await,
            "me".into(),
            named.encode(),
            Some("their-laptop".into()),
            SelfEnroll::Refuse,
            None,
            s.clone(),
            None,
            None,
        )
        .await
        .expect_err("the sample invite is undialable, so this still fails — just not on the name");
        assert_ne!(
            code_of(&e),
            Some(api::ERR_INVITE_NAME_CONFLICT),
            "an alias must RESOLVE the collision, not merely rename the error: {e:#}"
        );

        // …but an alias that ITSELF collides is refused identically. The alias does not bypass the
        // check — a duplicate display name makes our own `<peer>/<service>` routing ambiguous
        // whoever chose it, which is just as true of a name the local user picked.
        let fresh = sample_invite_for(inviter, 9_999_999_999);
        let e = redeem_invite(
            ep().await,
            "me".into(),
            fresh.encode(),
            Some("taken".into()),
            SelfEnroll::Refuse,
            None,
            s,
            None,
            None,
        )
        .await
        .expect_err("an alias that collides must still refuse");
        assert_eq!(
            code_of(&e),
            Some(api::ERR_INVITE_NAME_CONFLICT),
            "the collision check runs on the name we will STORE, alias or not: {e:#}"
        );

        // UNREACHABLE — an id-only address on the Minimal preset (no relay, no discovery), so
        // there is nothing to dial and it fails immediately. An unroutable IP would exercise the
        // same branch but spend 30s on a connect timeout in every CI run.
        let dead = sample_invite_for(inviter, 9_999_999_999);
        let e = redeem_invite(
            ep().await,
            "me".into(),
            dead.encode(),
            None,
            SelfEnroll::Refuse,
            None,
            store(),
            None,
            None,
        )
        .await
        .expect_err("an undialable inviter must refuse");
        assert_eq!(
            code_of(&e),
            Some(api::ERR_INVITER_UNREACHABLE),
            "the dial failure owns ERR_INVITER_UNREACHABLE: {e:#}"
        );
        assert!(
            e.to_string().contains("dial the inviter"),
            "and its MESSAGE must stay the string the porcelain matches on to explain a \
             self-redeem — rewording it made that branch dead code (#159 gate): {e}"
        );
    }

    /// #178: an enrollment line is refused when the caller did not offer that ceremony — and the
    /// refusal happens BEFORE the dial.
    ///
    /// The two halves are one test on purpose, because each alone is satisfiable by a wrong
    /// implementation:
    ///
    /// - Asserting only the refusal is satisfied by refusing enrollment lines outright, which
    ///   breaks #86. So the SAME line under `Allow` must get past the guard.
    /// - Asserting only "it errors" proves nothing about ORDER — every path through this function
    ///   errors on an undialable inviter. `sample_invite_for` names an id-only address on the
    ///   Minimal preset, so a line that reaches the dial fails with `ERR_INVITER_UNREACHABLE`.
    ///   Getting `ERR_SELF_ENROLL_NOT_OFFERED` instead is what proves nothing was contacted.
    ///
    /// Order is the security property, not a nicety: past the dial the secret is on the wire and
    /// the invite is burned, so a refusal there would cost the credential. Moving the guard below
    /// the `connect` flips this test's first code to `ERR_INVITER_UNREACHABLE`.
    #[tokio::test]
    async fn an_unoffered_self_enrollment_is_refused_before_the_dial() {
        use mcpmesh_local_api as api;
        let code_of = |e: &anyhow::Error| e.downcast_ref::<PairRefusal>().map(|r| r.code());
        let store = || {
            std::sync::Arc::new(
                crate::allowlist::PeerStore::open(
                    &tempfile::tempdir().unwrap().keep().join("s.redb"),
                )
                .unwrap(),
            )
        };
        let ep = || async {
            iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .bind()
                .await
                .unwrap()
        };
        let inviter = iroh::SecretKey::from_bytes(&[43u8; 32]).public();

        let mut enroll = sample_invite_for(inviter, 9_999_999_999);
        enroll.as_self = true;
        enroll.services = vec![]; // an enrollment grants nothing
        let line = enroll.encode();

        let e = redeem_invite(
            ep().await,
            "me".into(),
            line.clone(),
            None,
            SelfEnroll::Refuse,
            None,
            store(),
            None,
            None,
        )
        .await
        .expect_err("an enrollment line must be refused when the caller did not offer it");
        assert_eq!(
            code_of(&e),
            Some(api::ERR_SELF_ENROLL_NOT_OFFERED),
            "the guard must own its own code — and getting ERR_INVITER_UNREACHABLE here would \
             mean we dialled first, revealing the secret before deciding: {e:#}"
        );

        // The SAME line, offered: past the guard, so it now fails on the undialable inviter like
        // any other invite. Without this the guard could be a blanket refusal of every
        // `mcpmesh-enroll:` line — which passes the assertion above and breaks #86 entirely.
        let e = redeem_invite(
            ep().await,
            "me".into(),
            line,
            None,
            SelfEnroll::Allow,
            None,
            store(),
            None,
            None,
        )
        .await
        .expect_err("the sample inviter is undialable, so this still fails — just not on consent");
        assert_eq!(
            code_of(&e),
            Some(api::ERR_INVITER_UNREACHABLE),
            "with the ceremony offered, the guard must be OUT of the way: {e:#}"
        );

        // And the guard must not touch an ORDINARY invite: `Refuse` is the default every embedder
        // gets, so if it refused plain pairing lines it would break every caller.
        let plain = sample_invite_for(inviter, 9_999_999_999);
        let e = redeem_invite(
            ep().await,
            "me".into(),
            plain.encode(),
            None,
            SelfEnroll::Refuse,
            None,
            store(),
            None,
            None,
        )
        .await
        .expect_err("still undialable");
        assert_eq!(
            code_of(&e),
            Some(api::ERR_INVITER_UNREACHABLE),
            "an ordinary invite must be unaffected by the self-enrollment guard: {e:#}"
        );
    }

    /// #159, and the load-bearing half: the OPAQUE refusal gains a code without gaining
    /// information.
    ///
    /// The inviter answers one reason for unknown / expired / wrong secret deliberately —
    /// distinguishing them is a redemption oracle, since a prober would learn which guessed
    /// secrets were ever real. #159 asked for "expired vs already consumed" as separate codes;
    /// this is why the answer is no, and `ERR_INVITE_NOT_LIVE` (a fact about the INVITER, not the
    /// secret) is as close as it is safe to get.
    ///
    /// So: same code, same prose, whatever the underlying cause.
    #[test]
    fn the_opaque_refusal_gains_a_code_but_not_information() {
        // The three causes the inviter refuses to distinguish all arrive here identically — an
        // uncoded `PairReply::Refused` carrying REASON_REFUSED.
        let seen: Vec<(i64, String)> = [None, Some(RefusalCode::Unknown)]
            .into_iter()
            .map(|code| {
                let e = refusal_error(REASON_REFUSED, code);
                let refusal = e
                    .downcast_ref::<PairRefusal>()
                    .expect("the opaque refusal must still be branchable");
                (refusal.code(), e.to_string())
            })
            .collect();

        assert!(
            seen.iter()
                .all(|(c, _)| *c == mcpmesh_local_api::ERR_INVITE_REFUSED),
            "one code for the whole opaque family: {seen:?}"
        );
        assert!(
            seen.iter().all(|(_, m)| *m == seen[0].1),
            "and one MESSAGE — a per-cause message would rebuild the oracle in prose that the \
             code refuses to build in a number: {seen:?}"
        );
        assert!(
            !seen[0].1.contains("expired") && !seen[0].1.contains("unknown"),
            "the opaque reason must not name a cause at all: {}",
            seen[0].1
        );

        // And the collision refusal is NOT swallowed into it — that one is safe to distinguish,
        // because it only reaches a caller that already proved possession of a live secret.
        let collision = refusal_error("nickname taken", Some(RefusalCode::NicknameTaken));
        assert!(
            collision.downcast_ref::<NicknameTaken>().is_some(),
            "the recoverable collision keeps its own typed error"
        );
        assert!(
            collision.downcast_ref::<PairRefusal>().is_none(),
            "and must not be re-coded as the opaque refusal"
        );
    }

    /// #147 gate: the code means "rename and redeem the SAME invite again", so it may ride ONLY a
    /// refusal whose invite survived.
    ///
    /// The post-redeem race guard refuses the same collision with the invite already burned. The
    /// first implementation coded it too — every doc then told an embedder to send that user back
    /// to an invite that no longer exists, which is worse than the prose it replaced. This pairs
    /// the two send sites' prose with their coding decision so they cannot drift apart again.
    #[test]
    fn only_a_surviving_invite_earns_the_rename_and_retry_code() {
        // Through `collision_refusal`, which is what BOTH send sites call — not through the
        // pieces. Asserting on `reason_nickname_taken` + `refusal_error` separately passes even
        // when a send site pairs the wrong two, which is precisely the defect this pins.
        let PairReply::Refused { reason, code } = collision_refusal("studio-mac", true) else {
            panic!("expected a refusal");
        };
        assert!(reason.contains("redeem the same invite again"), "{reason}");
        assert_eq!(
            code,
            Some(RefusalCode::NicknameTaken),
            "the recoverable collision is the one that earns the code"
        );

        // The burned-invite collision. Its prose sends the user to a NEW invite, so a consumer
        // acting on the rename-and-retry code here would give the opposite of correct advice.
        let PairReply::Refused { reason, code } = collision_refusal("studio-mac", false) else {
            panic!("expected a refusal");
        };
        assert!(
            reason.contains("ask the inviter for a fresh invite"),
            "{reason}"
        );
        assert!(
            !reason.contains("redeem the same invite again"),
            "the two remedies must stay distinguishable: {reason}"
        );
        assert_eq!(
            code, None,
            "a burned invite must NOT carry the rename-and-retry code — an embedder writing copy \
             off it would send the user back to an invite that no longer exists"
        );
    }

    /// #147: the typed error's `Display` is the inviter's reason verbatim, so the wire message and
    /// the one `respond` renders into `ERR_NICKNAME_TAKEN` cannot drift. Re-deriving the sentence
    /// redeemer-side would be a second source of truth for the string this change exists to
    /// single-source — and the redeemer does not know whether the invite survived.
    #[test]
    fn the_typed_error_displays_the_inviters_reason_verbatim() {
        let wire = reason_nickname_taken("studio-mac", true);
        let e = NicknameTaken(format!("pairing refused: {wire}"));
        assert_eq!(e.to_string(), format!("pairing refused: {wire}"));
        assert!(e.to_string().contains("rename this node"));
    }

    /// #85 ask 3: the decision refuses a forged binding, a stranger, a revoked endpoint, and a
    /// node that has not opted in — and admits the real thing.
    ///
    /// The FORGED case is why this function exists separately from its I/O. `attest_to` always
    /// sends its own real endpoint id, so no honest client can present a binding for a *different*
    /// device — which means swapping the ceremony's TLS-authenticated id for the caller-claimed one
    /// went uncaught by every end-to-end test. Here the hello is built by hand.
    #[test]
    fn the_attestation_decision_refuses_everything_but_a_known_persons_real_device() {
        use crate::allowlist::{PeerEntry, PeerStore};
        use mcpmesh_trust::UserKey;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PeerStore::open(&dir.path().join("p.redb")).unwrap());
        let bob = UserKey::from_signing_key(mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(
            &[41u8; 32],
        ));
        let bob_uid = mcpmesh_trust::binding::user_id(&bob);
        let stranger = UserKey::from_signing_key(
            mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(&[42u8; 32]),
        );

        let (old_device, new_device) = ([1u8; 32], [2u8; 32]);
        store
            .add(PeerEntry {
                endpoint_id: old_device,
                nickname: "bob".into(),
                services: vec!["notes".into(), "files".into()],
                paired_at: None,
                user_id: Some(bob_uid.clone()),
                last_addr: None,
            })
            .unwrap();

        let ctx = |admit: bool| InviterCtx {
            store: store.clone(),
            invites: Arc::new(crate::pairing::LiveInvites::new()),
            config_path: dir.path().join("config.toml"),
            self_binding: None,
            grant: Box::new(|_, _, _| Box::pin(async { Ok(()) })),
            record_pairing: Box::new(|_| {}),
            audit_trust: Box::new(|_, _| {}),
            sign_binding: Box::new(|_| Box::pin(async { None })),
            admit_attested: admit,
            self_endpoint_id: [9u8; 32],
            self_nickname: "us".into(),
        };
        let hello = |pk: String, sig: String, claimed: [u8; 32]| RedeemerHello {
            secret: [0u8; 32],
            redeemer_id: claimed,
            redeemer_nickname: String::new(),
            user_pk: Some(pk),
            binding_sig: Some(sig),
            attest: true,
        };

        // THE happy case: bob's real binding for the NEW device.
        let (pk, sig) = mcpmesh_trust::binding::present(&bob, &new_device);
        let plan = attestation_decision(
            &hello(pk.clone(), sig.clone(), new_device),
            new_device,
            &ctx(true),
        )
        .expect("a known person's real device is admitted");
        assert_eq!(plan.user_id, bob_uid);
        assert_eq!(plan.nickname, "bob");
        assert_eq!(
            plan.services,
            vec!["notes".to_string(), "files".to_string()]
        );

        // FORGED: bob's binding for his OLD device, presented by the new one. The signature is
        // perfectly valid — it just does not bind THIS endpoint.
        let (old_pk, old_sig) = mcpmesh_trust::binding::present(&bob, &old_device);
        assert_eq!(
            attestation_decision(
                // …and the hello CLAIMS to be the old device, which is the lie the TLS id catches.
                &hello(old_pk, old_sig, old_device),
                new_device,
                &ctx(true)
            ),
            Err("binding does not verify against the authenticated endpoint"),
            "a binding for a DIFFERENT endpoint must not admit this one, however valid its \
             signature — verifying against the claimed id instead of the authenticated one is the \
             whole attack"
        );

        // STRANGER: a valid binding for this device, by an identity we have never seen.
        let (spk, ssig) = mcpmesh_trust::binding::present(&stranger, &new_device);
        assert_eq!(
            attestation_decision(&hello(spk, ssig, new_device), new_device, &ctx(true)),
            Err("no existing row for that identity"),
            "attestation admits another device of someone we already pair with — it is not itself \
             a pairing mechanism"
        );

        // NOT OPTED IN.
        assert_eq!(
            attestation_decision(
                &hello(pk.clone(), sig.clone(), new_device),
                new_device,
                &ctx(false)
            ),
            Err("not enabled on this node")
        );

        // REVOKED — and checked BEFORE the identity lookup succeeds is not enough: it must refuse a
        // device that would otherwise be admitted, which is exactly this one.
        store
            .revoke(crate::allowlist::RevokedEntry {
                endpoint_id: new_device,
                revoked_at: 1,
                reason: None,
                source: "signed".into(),
                signer_user_id: Some(bob_uid.clone()),
                issued_at: Some(1),
            })
            .unwrap();
        assert_eq!(
            attestation_decision(&hello(pk, sig, new_device), new_device, &ctx(true)),
            Err("endpoint is revoked"),
            "the ask-4 interlock: a device its owner declared stolen must not walk back in holding \
             a binding it still has"
        );
    }

    /// Services are the INTERSECTION across a person's devices, never the union.
    ///
    /// A new device must not arrive holding the most-privileged grant on the node. Pinned with
    /// DIFFERING, non-empty sets — with equal or empty ones the two operations agree and the test
    /// would measure nothing.
    #[test]
    fn attested_services_are_the_intersection_not_the_union() {
        use crate::allowlist::{PeerEntry, PeerStore};
        use mcpmesh_trust::UserKey;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PeerStore::open(&dir.path().join("p.redb")).unwrap());
        let bob = UserKey::from_signing_key(mcpmesh_trust::ed25519_dalek::SigningKey::from_bytes(
            &[41u8; 32],
        ));
        let bob_uid = mcpmesh_trust::binding::user_id(&bob);
        for (eid, services) in [
            ([1u8; 32], vec!["notes", "files", "shared"]),
            ([2u8; 32], vec!["notes", "shared", "phone-only"]),
        ] {
            store
                .add(PeerEntry {
                    endpoint_id: eid,
                    nickname: "bob".into(),
                    services: services.into_iter().map(String::from).collect(),
                    paired_at: None,
                    user_id: Some(bob_uid.clone()),
                    last_addr: None,
                })
                .unwrap();
        }
        let new_device = [3u8; 32];
        let (pk, sig) = mcpmesh_trust::binding::present(&bob, &new_device);
        let ctx = InviterCtx {
            store: store.clone(),
            invites: Arc::new(crate::pairing::LiveInvites::new()),
            config_path: dir.path().join("config.toml"),
            self_binding: None,
            grant: Box::new(|_, _, _| Box::pin(async { Ok(()) })),
            record_pairing: Box::new(|_| {}),
            audit_trust: Box::new(|_, _| {}),
            sign_binding: Box::new(|_| Box::pin(async { None })),
            admit_attested: true,
            self_endpoint_id: [9u8; 32],
            self_nickname: "us".into(),
        };
        let plan = attestation_decision(
            &RedeemerHello {
                secret: [0u8; 32],
                redeemer_id: new_device,
                redeemer_nickname: String::new(),
                user_pk: Some(pk),
                binding_sig: Some(sig),
                attest: true,
            },
            new_device,
            &ctx,
        )
        .expect("admitted");
        assert_eq!(
            plan.services,
            vec!["notes".to_string(), "shared".to_string()],
            "only what EVERY existing device of that person has — the union would hand the new \
             machine `files` and `phone-only`, which no single existing device holds together"
        );
    }

    /// An attestation offer round-trips, and MALFORMED input is refused rather than guessed at.
    ///
    /// Untested until the 0.46.0 gate: `.strip_prefix(ATTEST_SCHEME).or(Some(line))` — accepting
    /// any bare base64 line — went uncaught. The scheme prefix is what stops an invite line, an
    /// enrollment line or a revocation token being fed to the wrong ceremony.
    #[test]
    fn an_attestation_offer_round_trips_and_refuses_anything_else() {
        let offer = AttestOffer {
            node_id: [7u8; 32],
            node_addr_json: r#"{"id":"x","addrs":[]}"#.into(),
        };
        let line = offer.encode().unwrap();
        assert!(line.starts_with(ATTEST_SCHEME));
        assert_eq!(AttestOffer::decode(&line).unwrap(), offer);
        // Whitespace is forgiven — it is a pasted line.
        assert_eq!(AttestOffer::decode(&format!("  {line}\n")).unwrap(), offer);

        for bad in [
            "",
            "hello",
            // The right shape, the WRONG scheme: an invite, an enrollment link, a revocation
            // token. Each is a real artifact a user could paste here by mistake.
            &line.replace(ATTEST_SCHEME, "mcpmesh-invite:"),
            &line.replace(ATTEST_SCHEME, "mcpmesh-enroll:"),
            &line.replace(ATTEST_SCHEME, "mcpmesh-revoke:"),
            // Right scheme, junk body.
            &format!("{ATTEST_SCHEME}!!!not-base64!!!"),
            ATTEST_SCHEME,
        ] {
            assert!(
                AttestOffer::decode(bad).is_err(),
                "{bad:?} must be refused, not guessed at"
            );
        }
    }

    /// #223 (review, item 8): an offer / invite whose embedded address names a DIFFERENT endpoint
    /// than the id the revocation check read is refused BEFORE the dial. The address's endpoint is a
    /// real, counting listener, so an unrefused dial would complete a handshake with it — which
    /// already identifies this node to it, before the post-handshake address-swap check ever runs.
    ///
    /// Deleting the pre-dial id check in `attest_to` fails the attest zero count (and its message
    /// then comes from the post-handshake check); deleting it in `redeem_invite` fails that count.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_mismatched_address_id_is_refused_before_the_dial() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            let decoy = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .alpns(vec![mcpmesh_net::ALPN_PAIR.to_vec()])
                .bind()
                .await
                .unwrap();
            let dials = Arc::new(AtomicUsize::new(0));
            let (e, c) = (decoy.clone(), dials.clone());
            let _accept = tokio::spawn(async move {
                while let Some(incoming) = e.accept().await {
                    if let Ok(conn) = incoming.await {
                        c.fetch_add(1, Ordering::SeqCst);
                        conn.close(0u32.into(), b"test");
                    }
                }
            });
            let claimed = iroh::SecretKey::from_bytes(&[44u8; 32]).public();
            assert_ne!(claimed, decoy.id(), "fixture: the two ids must differ");
            let decoy_addr = serde_json::to_string(&decoy.addr()).unwrap();
            let me = || async {
                iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                    .relay_mode(iroh::RelayMode::Disabled)
                    .bind()
                    .await
                    .unwrap()
            };
            let store = || {
                Arc::new(
                    crate::allowlist::PeerStore::open(
                        &tempfile::tempdir().unwrap().keep().join("s.redb"),
                    )
                    .unwrap(),
                )
            };

            let offer = AttestOffer {
                node_id: *claimed.as_bytes(),
                node_addr_json: decoy_addr.clone(),
            }
            .encode()
            .unwrap();
            let binding = Some(SelfBinding {
                user_pk: "b64u:me".into(),
                sig: "sig".into(),
            });
            let e = attest_to(me().await, offer, store(), binding, None)
                .await
                .expect_err("a mismatched offer must be refused");
            assert!(format!("{e:#}").contains("peer id mismatch"), "{e:#}");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            assert_eq!(
                dials.load(Ordering::SeqCst),
                0,
                "attest_to must not dial the decoy"
            );

            let mut invite = sample_invite_for(claimed, 9_999_999_999);
            invite.inviter_addr_json = decoy_addr;
            let e = redeem_invite(
                me().await,
                "me".into(),
                invite.encode(),
                None,
                SelfEnroll::Refuse,
                None,
                store(),
                None,
                None,
            )
            .await
            .expect_err("a mismatched invite must be refused");
            assert_eq!(
                e.downcast_ref::<PairRefusal>().map(|r| r.code()),
                Some(mcpmesh_local_api::ERR_INVITER_MISMATCH),
                "{e:#}"
            );
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            assert_eq!(
                dials.load(Ordering::SeqCst),
                0,
                "redeem_invite must not dial the decoy"
            );
        })
        .await
        .expect("mismatch test timed out");
    }

    /// #223 (review): `attest_to` must not dial a node this node has REVOKED — attesting hands it
    /// this person's identity binding. `redeem_invite` checks before its dial; this had no check.
    ///
    /// The offering node is a real endpoint that counts handshakes, so the CONTROL (the same offer,
    /// unrevoked) proves the dial lands; the refusal carries `ERR_PRINCIPAL_REVOKED`, like
    /// redemption's. Deleting the check fails the code assertion and the zero count.
    #[tokio::test(flavor = "multi_thread")]
    async fn attesting_to_a_revoked_node_is_refused_before_the_dial() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            let offering = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .alpns(vec![mcpmesh_net::ALPN_PAIR.to_vec()])
                .bind()
                .await
                .unwrap();
            let dials = Arc::new(AtomicUsize::new(0));
            let (e, c) = (offering.clone(), dials.clone());
            let _accept = tokio::spawn(async move {
                while let Some(incoming) = e.accept().await {
                    if let Ok(conn) = incoming.await {
                        c.fetch_add(1, Ordering::SeqCst);
                        conn.close(0u32.into(), b"test");
                    }
                }
            });
            let offer = AttestOffer {
                node_id: *offering.id().as_bytes(),
                node_addr_json: serde_json::to_string(&offering.addr()).unwrap(),
            }
            .encode()
            .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let store =
                Arc::new(crate::allowlist::PeerStore::open(&dir.path().join("s.redb")).unwrap());
            let binding = || {
                Some(SelfBinding {
                    user_pk: "b64u:me".into(),
                    sig: "sig".into(),
                })
            };
            let me = || async {
                iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                    .relay_mode(iroh::RelayMode::Disabled)
                    .bind()
                    .await
                    .unwrap()
            };

            store
                .revoke(crate::allowlist::RevokedEntry {
                    endpoint_id: *offering.id().as_bytes(),
                    revoked_at: 1,
                    reason: None,
                    source: "local".into(),
                    signer_user_id: None,
                    issued_at: None,
                })
                .unwrap();
            let e = attest_to(me().await, offer.clone(), store.clone(), binding(), None)
                .await
                .expect_err("attesting to a revoked node must be refused");
            assert_eq!(
                e.downcast_ref::<PairRefusal>().map(|r| r.code()),
                Some(mcpmesh_local_api::ERR_PRINCIPAL_REVOKED),
                "{e:#}"
            );
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            assert_eq!(dials.load(Ordering::SeqCst), 0, "nothing may be contacted");

            // The control: lifted, the same offer is dialled.
            store.unrevoke(offering.id().as_bytes()).unwrap();
            let _ = attest_to(me().await, offer, store, binding(), None).await;
            assert!(
                dials.load(Ordering::SeqCst) > 0,
                "control: an unrevoked offering node is dialled"
            );
        })
        .await
        .expect("attest revocation test timed out");
    }

    /// A hello with NO binding is refused — the arm that has no other coverage.
    #[test]
    fn an_attestation_without_a_binding_is_refused() {
        use crate::allowlist::PeerStore;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PeerStore::open(&dir.path().join("p.redb")).unwrap());
        let ctx = InviterCtx {
            store,
            invites: Arc::new(crate::pairing::LiveInvites::new()),
            config_path: dir.path().join("config.toml"),
            self_binding: None,
            grant: Box::new(|_, _, _| Box::pin(async { Ok(()) })),
            record_pairing: Box::new(|_| {}),
            audit_trust: Box::new(|_, _| {}),
            sign_binding: Box::new(|_| Box::pin(async { None })),
            admit_attested: true,
            self_endpoint_id: [9u8; 32],
            self_nickname: "us".into(),
        };
        for (pk, sig) in [
            (None, None),
            (Some("b64u:whatever".to_string()), None),
            (None, Some("sig".to_string())),
        ] {
            assert_eq!(
                attestation_decision(
                    &RedeemerHello {
                        secret: [0u8; 32],
                        redeemer_id: [1u8; 32],
                        redeemer_nickname: String::new(),
                        user_pk: pk,
                        binding_sig: sig,
                        attest: true,
                    },
                    [1u8; 32],
                    &ctx
                ),
                Err("no binding presented")
            );
        }
    }
}

/// The `mcpmesh-attest:` scheme — where to dial for a device attestation (#85 ask 3).
pub const ATTEST_SCHEME: &str = "mcpmesh-attest:";

/// An attestation OFFER: where to dial, and whose node it is.
///
/// **Carries nothing secret.** No bearer credential, no expiry, no burn — an invite's `secret` is
/// what admits a stranger, and an attestation admits nobody who cannot already produce a binding to
/// a `user_id` the offering node pairs with. The line exists only because a device freshly restored
/// from a recovery phrase holds no rows and therefore has no way to find anyone.
///
/// The same two fields ride an invite line in the clear today, so this discloses nothing new.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AttestOffer {
    /// The offering node's endpoint id — the redeemer verifies the TLS peer id against it, the same
    /// address-swap defense an invite has.
    pub node_id: [u8; 32],
    /// Its `EndpointAddr` as JSON, so the ceremony needs no discovery (works on a LAN, or
    /// localhost).
    pub node_addr_json: String,
}

impl AttestOffer {
    pub fn encode(&self) -> anyhow::Result<String> {
        Ok(format!(
            "{ATTEST_SCHEME}{}",
            data_encoding::BASE64URL_NOPAD.encode(&serde_json::to_vec(self)?)
        ))
    }

    pub fn decode(line: &str) -> anyhow::Result<Self> {
        let body = line
            .trim()
            .strip_prefix(ATTEST_SCHEME)
            .context("not an attestation offer (expected a mcpmesh-attest: line)")?;
        let raw = data_encoding::BASE64URL_NOPAD
            .decode(body.as_bytes())
            .context("attestation offer is not valid base64url")?;
        serde_json::from_slice(&raw).context("attestation offer is malformed")
    }
}

/// Present THIS device's binding to a peer that already pairs with this person (#85 ask 3).
///
/// The mirror of [`redeem_invite`]: same ALPN, same framing, same hello — with `attest: true` and no
/// secret. The peer verifies our binding against our TLS-authenticated endpoint and, if it already
/// holds a row for that `user_id`, admits us as another device of that person.
///
/// Requires a `self_binding`: with no user key there is nothing to attest, and sending a hello that
/// could only ever be refused wastes a rate-limited connection on both nodes.
///
/// Writes the peer's row on success, so the ceremony leaves this device able to dial back — a
/// restored machine holds no rows at all, which is the situation this exists for.
pub async fn attest_to(
    endpoint: iroh::Endpoint,
    offer_line: String,
    store: Arc<PeerStore>,
    self_binding: Option<SelfBinding>,
    nickname_for_peer: Option<String>,
) -> anyhow::Result<PairResult> {
    let offer = AttestOffer::decode(&offer_line)?;
    let binding = self_binding.context(
        "this device has no user key, so it has nothing to attest — import your recovery phrase \
         first (`mcpmesh identity import`)",
    )?;
    // #223, as `redeem_invite` does it: BEFORE the dial. Attesting sends this person's user key
    // binding to the offering node, and a node this node has REVOKED must not receive it.
    let (s, id) = (store.clone(), offer.node_id);
    let refused = tokio::task::spawn_blocking(move || s.is_refused(&id))
        .await
        .context("join attesting peer revocation check")?;
    if refused {
        bail!(PairRefusal::new(
            mcpmesh_local_api::ERR_PRINCIPAL_REVOKED,
            "the offering node is revoked on this node, so attesting to it would hand your identity \
             binding to a device you declared compromised — nothing was contacted. Lift the \
             revocation first (peer_unrevoke) if it was a mistake",
        ));
    }
    let addr: iroh::EndpointAddr = serde_json::from_str(&offer.node_addr_json)
        .context("attestation offer carries an unusable address")?;
    // #203, same as `redeem_invite`: an offer's addresses are the remote party's claim, dialled
    // before storage.
    let addr = crate::daemon::dial::dialable_only(addr);
    // #223 review: the revocation check above is on `offer.node_id`; the dial goes to `addr.id`. An
    // offer whose two disagree must not dial at all — the post-handshake check below runs only after
    // the handshake has already reached, and identified us to, whatever `addr.id` names.
    anyhow::ensure!(
        addr.id.as_bytes() == &offer.node_id,
        "peer id mismatch — refusing (address-swap defense)"
    );
    let conn = endpoint
        .connect(addr, mcpmesh_net::ALPN_PAIR)
        .await
        .context("dial the attesting peer")?;
    // Address-swap defense, exactly as `redeem_invite` does it: the TLS peer id is authoritative,
    // and an offer that routed us somewhere else is refused before we send anything.
    anyhow::ensure!(
        conn.remote_id().as_bytes() == &offer.node_id,
        "peer id mismatch — refusing (address-swap defense)"
    );

    let hello = RedeemerHello {
        // No invite. Zeroed rather than random: it is never read on this path, and a random value
        // would look like a credential to anyone reading a capture.
        secret: [0u8; 32],
        redeemer_id: *endpoint.id().as_bytes(),
        redeemer_nickname: String::new(),
        user_pk: Some(binding.user_pk.clone()),
        binding_sig: Some(binding.sig.clone()),
        attest: true,
    };
    let (mut send, recv) = conn
        .open_bi()
        .await
        .context("open the attestation stream")?;
    write_frame(&mut send, &serde_json::to_value(&hello)?)
        .await
        .context("send the attestation hello")?;
    let mut reader = FrameReader::new(BufReader::new(recv), MAX_PAIR_FRAME);
    let reply: PairReply = match reader.next().await.context("read the attestation reply")? {
        Some(Inbound::Frame(v)) => {
            serde_json::from_value(v).context("peer reply is not a PairReply")?
        }
        _ => anyhow::bail!("no reply from the peer (connection closed before a reply)"),
    };
    let (peer_id, peer_nickname, peer_pk, peer_sig) = match reply {
        PairReply::Ok {
            inviter_id,
            inviter_nickname,
            user_pk,
            binding_sig,
        } => (inviter_id, inviter_nickname, user_pk, binding_sig),
        PairReply::Refused { reason, .. } => anyhow::bail!(
            "the peer refused this attestation ({reason}). It admits another of your devices only \
             if it already pairs with you AND has `admit_attested_devices` enabled"
        ),
    };
    anyhow::ensure!(
        peer_id == offer.node_id,
        "peer reported an id that disagrees with its own offer"
    );

    // Their binding is verified against THEIR endpoint — ours to check, exactly as the pairing path
    // does. An unverifiable one stores no `user_id` rather than failing the ceremony: their identity
    // is a bonus here, not the credential.
    let peer_user_id = match (peer_pk.as_deref(), peer_sig.as_deref()) {
        (Some(pk), Some(sig)) => mcpmesh_trust::binding::verify_presented(pk, sig, &peer_id).ok(),
        _ => None,
    };
    // MERGE, never clobber (#85 ask 3 gate).
    //
    // `store.add` is an upsert keyed on endpoint id, and this device may already pair with this
    // peer — re-attesting is ordinary. The first cut wrote a fresh row every time, which wiped the
    // peer's `services`, renamed it, and could DOWNGRADE a proven `user_id` to `None`, the one
    // thing `allowlist.rs` says the pairing path must never do. The pairing path merges for exactly
    // these reasons; this now follows it.
    let existing = store.resolve(&peer_id)?;
    let nickname = nickname_for_peer
        .or_else(|| existing.as_ref().map(|e| e.nickname.clone()))
        .unwrap_or(peer_nickname);
    // #203: what THIS connection observed, on the same rule as `redeem_invite` and
    // `dial_hint::refresh` — never the offer's address list.
    //
    // This previously stored `endpoint_addr_of(&conn)`, which is `EndpointAddr::from(remote_id)`:
    // a hint with NO addresses. That is not merely useless, it is destructive — being `Some` it
    // won the `or_else` below and REPLACED whatever proven hint the peer already had with an empty
    // one. `observed_for` returns `None` when it learned nothing, so the existing hint survives.
    let addr_json = crate::daemon::dial_hint::observed_for(&conn);
    store.add(PeerEntry {
        endpoint_id: peer_id,
        nickname: nickname.clone(),
        services: existing
            .as_ref()
            .map(|e| e.services.clone())
            .unwrap_or_default(),
        paired_at: existing
            .as_ref()
            .and_then(|e| e.paired_at.clone())
            .or_else(|| Some(epoch_now().to_string())),
        // Never downgrade a proven identity to `None`.
        user_id: peer_user_id
            .clone()
            .or_else(|| existing.as_ref().and_then(|e| e.user_id.clone())),
        // …nor a known dial hint. Under `relay_mode = "disabled"` losing it is unrecoverable
        // without another ceremony.
        last_addr: crate::daemon::dial_hint::merge_hint(
            addr_json,
            existing.as_ref().and_then(|e| e.last_addr.clone()),
        ),
    })?;
    conn.close(0u32.into(), b"done");
    Ok(PairResult {
        peer_nickname: nickname,
        // No SAS: the whole point is that a binding replaces the human ceremony. An empty string
        // rather than a fabricated code — a caller rendering one would be showing the operator a
        // number that verified nothing.
        sas_code: String::new(),
        app_label: None,
        peer_user_id,
        enrolled_as_self: false,
        services: Vec::new(),
    })
}
