//! A no-iroh mcpmesh-local/1 client: connect the UDS, read the server's `Hello`
//! first frame, assert the api name, then issue typed request/response frames. Distinct
//! from the CLI crate (`cli/`)'s ControlClient (which uses mcpmesh_net::framing) — this one links no
//! iroh, so kb and the host shell can use it. kb calls this to self-register
//! its `[services.kb]` socket backend with the running mcpmesh daemon.
use std::path::Path;

use serde_json::Value;

use crate::codec::{FrameReader, Inbound, MAX_FRAME_BYTES, write_frame};
use crate::protocol::{
    AuditSummaryResult, BackendSpec, BlobFetchCancelParams, BlobFetchCancelResult, BlobFetchParams,
    BlobFetchResult, BlobGrantParams, BlobPublishParams, BlobPublishResult, BlobScopeList, Hello,
    InviteParams, InviteResult, OpenSessionParams, OrgJoinParams, OrgJoinResult, PairParams,
    PairResult, PeerDiagnosticsParams, PeerDiagnosticsResult, PeerEndorseParams, PeerEndorseResult,
    PeerHintClearParams, PeerHintClearResult, PeerIntroduceParams, PeerRemoveParams,
    PeerRenameParams, PeerServicesParams, PeerServicesResult, RegisterServiceParams, Request,
    RosterInstallParams, RosterInstallResult, ServiceAllowParams, SetAppMetadataParams,
    SetNicknameParams, SetRelaysParams, SetRelaysResult, SetRosterUrlParams, StatusResult,
    StreamFrame, UnregisterServiceParams,
};
use crate::transport::{connect_local, split_local};

/// The client's read half — boxed so ONE `ControlClient` serves every transport (the
/// platform socket/pipe via [`connect_control`], or an embedder's in-memory duplex via
/// [`connect_control_io`]).
pub type ControlRead = Box<dyn tokio::io::AsyncRead + Send + Unpin>;
/// The client's write half — see [`ControlRead`].
pub type ControlWrite = Box<dyn tokio::io::AsyncWrite + Send + Unpin>;

/// A connected mcpmesh-local/1 client: the framed stream + the server's `Hello`.
pub struct ControlClient {
    hello: Hello,
    reader: FrameReader<ControlRead>,
    writer: ControlWrite,
}

/// Hand-rolled (the boxed transport halves are not `Debug`): the `Hello` is the one
/// diagnostic a `{:?}` needs — tests format `Result<ControlClient, _>` this way.
impl std::fmt::Debug for ControlClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlClient")
            .field("hello", &self.hello)
            .finish_non_exhaustive()
    }
}

/// The error surface of the client — thin, so callers can `anyhow`-wrap it.
///
/// The `Display`/`Error`/`From` impls below are hand-rolled rather than derived: the
/// `client` feature deliberately pulls ONLY tokio (no `thiserror`), and the hand-rolled
/// impls are behavior-identical (same messages, same `?`-conversion from `io::Error`)
/// with zero extra dependencies.
#[derive(Debug)]
pub enum ClientError {
    Io(std::io::Error),
    Closed(&'static str),
    Malformed(&'static str),
    WrongApi { got: String, want: &'static str },
    Api(Value),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(err) => write!(f, "io: {err}"),
            ClientError::Closed(what) => write!(f, "connection closed before {what}"),
            ClientError::Malformed(what) => write!(f, "malformed {what} frame"),
            ClientError::WrongApi { got, want } => {
                write!(f, "unexpected api: got {got:?}, want {want:?}")
            }
            ClientError::Api(err) => write!(f, "control API error: {err}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<std::io::Error> for ClientError {
    fn from(err: std::io::Error) -> Self {
        ClientError::Io(err)
    }
}

impl ControlClient {
    pub fn hello(&self) -> &Hello {
        &self.hello
    }

    /// Issue a typed request; return the JSON-RPC `result` (or `ClientError::Api` on a
    /// JSON-RPC `error`).
    pub async fn request(&mut self, request: Request) -> Result<Value, ClientError> {
        let frame = serde_json::to_value(&request).expect("Request serializes");
        self.request_value(&frame).await
    }

    /// Issue a RAW request frame — the escape hatch for methods outside the typed
    /// [`Request`] surface (the daemon-internal `shutdown`, third-party
    /// `{"method":..,"params":{}}` shapes the dispatcher tolerates). Returns the JSON-RPC
    /// `result` value (or `ClientError::Api` on a JSON-RPC `error`).
    pub async fn request_value(&mut self, request: &Value) -> Result<Value, ClientError> {
        write_frame(&mut self.writer, request).await?;
        match self.reader.next().await? {
            Some(Inbound::Frame(resp)) => {
                if let Some(err) = resp.get("error") {
                    return Err(ClientError::Api(err.clone()));
                }
                Ok(resp.get("result").cloned().unwrap_or(Value::Null))
            }
            Some(Inbound::Violation(_)) => Err(ClientError::Malformed("response")),
            None => Err(ClientError::Closed("response")),
        }
    }

    /// Send a request WITHOUT reading a response — for `OpenSession`, after which the
    /// socket stops being JSON-RPC and becomes a raw MCP byte pipe (protocol.rs). Returns
    /// the framed halves so the caller can pump the session — the SAME `FrameReader` that
    /// read the Hello, so bytes the daemon pipelined behind it are never lost. A caller
    /// that must re-box the read half calls `FrameReader::into_inner`, which returns the
    /// BUFFERED reader (its read-ahead travels with it — see the pipelining test below).
    pub async fn open_session(
        self,
        peer: String,
        service: String,
    ) -> Result<(FrameReader<ControlRead>, ControlWrite), ClientError> {
        self.open_session_with_idle_timeout(peer, service, None)
            .await
    }

    /// [`open_session`](Self::open_session) with a per-session QUIC idle timeout (#166).
    ///
    /// See [`OpenSessionParams::idle_timeout_secs`] for what it can and cannot do — in short, it
    /// can always make this session die sooner when it goes quiet, and can never make it outlive
    /// what the peer allows.
    ///
    /// [`OpenSessionParams::idle_timeout_secs`]: crate::protocol::OpenSessionParams::idle_timeout_secs
    pub async fn open_session_with_idle_timeout(
        mut self,
        peer: String,
        service: String,
        idle_timeout_secs: Option<u64>,
    ) -> Result<(FrameReader<ControlRead>, ControlWrite), ClientError> {
        let frame = serde_json::to_value(Request::OpenSession(OpenSessionParams {
            peer,
            service,
            idle_timeout_secs,
        }))
        .expect("Request serializes");
        write_frame(&mut self.writer, &frame).await?;
        Ok((self.reader, self.writer))
    }

    /// Send a parameterless stream-upgrade request WITHOUT reading a response — like
    /// [`open_session`](Self::open_session), but generic on the `method`: after this call the
    /// socket stops being request/response and becomes a one-way push stream of frames the caller
    /// READS (the `subscribe` telemetry surface). Returns the framed halves — the SAME
    /// `FrameReader` that read the Hello, so any frame the daemon pipelined behind it is never
    /// lost. The write half is handed back so the caller can hold the connection open (a watcher
    /// only reads, but dropping the writer would half-close the socket).
    pub async fn open_stream(
        mut self,
        method: &str,
    ) -> Result<(FrameReader<ControlRead>, ControlWrite), ClientError> {
        let frame = serde_json::json!({ "method": method });
        write_frame(&mut self.writer, &frame).await?;
        Ok((self.reader, self.writer))
    }

    /// Issue `request` and deserialize the JSON-RPC `result` into `T` — the shared core of every
    /// typed helper below. `what` names the result in the [`ClientError::Malformed`] surface. The
    /// wrong-type hazard the raw [`request`](Self::request) leaves to the caller is closed here:
    /// each helper pairs its Request variant with its result type once, in this crate.
    async fn request_typed<T: serde::de::DeserializeOwned>(
        &mut self,
        request: Request,
        what: &'static str,
    ) -> Result<T, ClientError> {
        let v = self.request(request).await?;
        serde_json::from_value(v).map_err(|_| ClientError::Malformed(what))
    }

    /// Issue `request` and discard the ack body (the daemon answers `{}` for verbs with no result
    /// vocabulary). A JSON-RPC error still surfaces as [`ClientError::Api`].
    async fn request_ack(&mut self, request: Request) -> Result<(), ClientError> {
        self.request(request).await.map(|_| ())
    }

    /// The daemon's `status` picture: services served, known peers, roster/presence state,
    /// self identity, recent pairings, and advisory reachability.
    pub async fn status(&mut self) -> Result<StatusResult, ClientError> {
        self.request_typed(Request::Status, "status result").await
    }

    /// Register/update a `[services.*]` entry idempotently (the daemon persists it and hot-reloads
    /// serving). The daemon acks; the ack body is discarded.
    pub async fn register_service(
        &mut self,
        name: &str,
        backend: BackendSpec,
        allow: Vec<String>,
    ) -> Result<(), ClientError> {
        self.register_service_with(name, backend, allow, false)
            .await
    }

    /// [`register_service`](Self::register_service) with an explicit `ephemeral` flag (#36). When
    /// `ephemeral` is true the registration lives only in daemon memory and is unregistered
    /// automatically when THIS control connection closes — no config write, nothing to clean up.
    /// Ideal for an embedder serving a `socket` backend from a fresh path each run.
    pub async fn register_service_with(
        &mut self,
        name: &str,
        backend: BackendSpec,
        allow: Vec<String>,
        ephemeral: bool,
    ) -> Result<(), ClientError> {
        self.request_ack(Request::RegisterService(RegisterServiceParams {
            name: name.to_string(),
            backend,
            allow,
            ephemeral,
            rate_limit_per_min: None,
        }))
        .await
    }

    /// Mint a single-use pairing invite granting `services` (see `invite_multi` for more than
    /// one); return the copyable
    /// `mcpmesh-invite:` line + its expiry.
    pub async fn invite(&mut self, services: Vec<String>) -> Result<InviteResult, ClientError> {
        self.invite_with(services, None).await
    }

    /// [`invite`](Self::invite) with an opaque `app_label` (#31) carried through to the redeemer's
    /// `pair` result. mcpmesh never interprets the label; the embedder does (e.g. its own URN).
    pub async fn invite_with(
        &mut self,
        services: Vec<String>,
        app_label: Option<String>,
    ) -> Result<InviteResult, ClientError> {
        self.invite_multi(services, app_label, None).await
    }

    /// `invite_with`, plus `max_uses` (#87): an invite redeemable up to that many times, each
    /// redemption running its own SAS ceremony and writing its own peer rows.
    ///
    /// `None` = 1, the single-use default. The value is clamped daemon-side to
    /// [`MAX_INVITE_USES`](crate::MAX_INVITE_USES) — read
    /// [`InviteResult::uses_remaining`](crate::InviteResult::uses_remaining) for what you actually
    /// got rather than assuming the request was honoured verbatim.
    pub async fn invite_multi(
        &mut self,
        services: Vec<String>,
        app_label: Option<String>,
        max_uses: Option<u32>,
    ) -> Result<InviteResult, ClientError> {
        self.invite_named(services, app_label, max_uses, None).await
    }

    /// Mint an invite, optionally under YOUR OWN local name for whoever redeems it (#87).
    ///
    /// `peer_nickname` overrides the name they claim for themselves — the fix for two same-model
    /// machines that does not require the other person to rename theirs. Never sent to them, and
    /// rejected alongside `max_uses > 1` (one name for every redeemer collides on the second).
    pub async fn invite_named(
        &mut self,
        services: Vec<String>,
        app_label: Option<String>,
        max_uses: Option<u32>,
        peer_nickname: Option<String>,
    ) -> Result<InviteResult, ClientError> {
        self.invite_full(services, app_label, max_uses, peer_nickname, false)
            .await
    }

    /// Mint an invite, optionally as a SELF-ENROLLMENT (#86): the redeemer becomes another device
    /// of YOU rather than a peer, so both present one identity.
    ///
    /// `as_self` requires an empty `services` and `max_uses` of 1 — it grants nothing, and a
    /// multi-use identity invite is a standing offer to become you.
    pub async fn invite_full(
        &mut self,
        services: Vec<String>,
        app_label: Option<String>,
        max_uses: Option<u32>,
        peer_nickname: Option<String>,
        as_self: bool,
    ) -> Result<InviteResult, ClientError> {
        self.request_typed(
            Request::Invite(InviteParams {
                services,
                app_label,
                max_uses,
                peer_nickname,
                as_self,
            }),
            "invite result",
        )
        .await
    }

    /// Produce an endorsement of `subject` for someone else to redeem (#65).
    ///
    /// Signs with THIS node's user key. It is a statement for the recipient — it changes nothing
    /// about your own trust in the subject, and only resolves for someone paired with you. Refused
    /// when this node holds no user key of its own (enrolled, or none on disk — `api_minor >= 63`).
    pub async fn endorse_peer(
        &mut self,
        subject: &str,
        subject_user_id: Option<String>,
    ) -> Result<PeerEndorseResult, ClientError> {
        self.request_typed(
            Request::PeerEndorse(PeerEndorseParams {
                subject: subject.to_string(),
                subject_user_id,
            }),
            "peer endorse result",
        )
        .await
    }

    /// Dump the durable per-peer state this node carries for `peer` (#140), `api_minor >= 33`.
    ///
    /// The persisted dial hint verbatim, whether the DIAL actually uses it, the addresses inside it,
    /// iroh's own view (`api_minor >= 56`), the pairing stamp and the live reachability row — one
    /// capture, intended to be run on BOTH ends of a stuck pairing and compared.
    ///
    /// **Read-only, but NOT inert.** It probes nothing, dials nothing and writes nothing — but
    /// reading iroh's view delivers a message to that remote's actor, which resets its ~60s idle
    /// timer. Polling this keeps remote state alive (a selected path included) that would otherwise
    /// be reaped.
    ///
    /// That matters for the one experiment this method exists to support: #140's step A is "close
    /// every session, **wait >60s**, then probe". Poll during the wait and you preserve the very
    /// state you are trying to clear, and get a false negative.
    ///
    /// Existed as a verb since 0.35.0 with no typed method here, so an embedder had to hand-build
    /// the request — which the one embedder who needs it most (a node embedder debugging #140)
    /// would have to do at exactly the wrong moment.
    pub async fn peer_diagnostics(
        &mut self,
        peer: &str,
    ) -> Result<PeerDiagnosticsResult, ClientError> {
        self.request_typed(
            Request::PeerDiagnostics(PeerDiagnosticsParams {
                peer: peer.to_string(),
            }),
            "peer diagnostics result",
        )
        .await
    }

    /// Forget this node's stored dial hint for `peer` (#140), `api_minor >= 59`.
    ///
    /// The hint is the only durable per-peer state on this node's disk that the dial path reads, and
    /// the only thing a long-lived pairing carries that a freshly paired identity does not — so
    /// clearing it makes the pairing address like a fresh one. Advisory, never authorization: the
    /// peer row, its `user_id`, its services and its pairing stamp are untouched, and an absent hint
    /// is a supported state (the dial degrades to id-only). Errors for an unknown peer; `cleared`
    /// is `false` for a known peer that had no hint.
    pub async fn peer_hint_clear(
        &mut self,
        peer: &str,
    ) -> Result<PeerHintClearResult, ClientError> {
        self.request_typed(
            Request::PeerHintClear(PeerHintClearParams {
                peer: peer.to_string(),
            }),
            "peer hint clear result",
        )
        .await
    }

    /// Install a peer from an endorsement by someone you are already paired with (#65).
    ///
    /// Installs IDENTITY, not authorization — the peer becomes resolvable and is granted nothing.
    /// `subject_user_id` requires `subject_binding`, the subject's OWN device→user binding: a
    /// `user_id` is authorization-bearing and public, so an endorser alone must not attach one.
    pub async fn introduce_peer(&mut self, params: PeerIntroduceParams) -> Result<(), ClientError> {
        self.request_ack(Request::PeerIntroduce(params)).await
    }

    /// Redeem a pairing invite; return the inviter's suggested nickname, the display-only SAS
    /// code, and the granted services.
    pub async fn pair(&mut self, invite_line: &str) -> Result<PairResult, ClientError> {
        self.pair_as(invite_line, None).await
    }

    /// Redeem an invite, optionally under YOUR OWN local name for the inviter (#87).
    ///
    /// `as_nickname` overrides the name the invite suggests. Use it when that name is already
    /// taken locally — otherwise the pairing is refused and the only other fixes are asking the
    /// inviter to re-mint or renaming your existing peer. It does not bypass the collision check:
    /// an alias that itself collides is refused the same way.
    pub async fn pair_as(
        &mut self,
        invite_line: &str,
        as_nickname: Option<String>,
    ) -> Result<PairResult, ClientError> {
        self.pair_opts(invite_line, as_nickname, false).await
    }

    /// Redeem an invite, stating whether a SELF-ENROLLMENT is a ceremony you offered (#178).
    ///
    /// [`pair`](Self::pair) and [`pair_as`](Self::pair_as) pass `false`, so a `mcpmesh-enroll:` line
    /// pasted into an ordinary "join" field is refused with
    /// [`ERR_SELF_ENROLL_NOT_OFFERED`](crate::ERR_SELF_ENROLL_NOT_OFFERED) before anything is
    /// dialled — the invite survives, so the same line still works once the person is offered the
    /// real choice. Pass `true` only from a path that actually means "add another of my own
    /// devices": the ceremony writes a device→user binding that is irrevocable short of rotating
    /// the user key.
    ///
    /// `mcpmesh_node::pairing::is_enrollment_line` answers which kind of line you are holding without
    /// dialling, for a UI that wants to PROMPT rather than recover from a refusal.
    pub async fn pair_opts(
        &mut self,
        invite_line: &str,
        as_nickname: Option<String>,
        allow_self_enroll: bool,
    ) -> Result<PairResult, ClientError> {
        self.request_typed(
            Request::Pair(PairParams {
                invite_line: invite_line.to_string(),
                as_nickname,
                allow_self_enroll,
            }),
            "pair result",
        )
        .await
    }

    /// Unpair a peer by nickname: drops its identity row AND its every-`allow` membership
    /// (idempotent; live sessions are not severed). The daemon acks; the ack body is discarded.
    pub async fn peer_remove(&mut self, nickname: &str) -> Result<(), ClientError> {
        self.request_ack(Request::PeerRemove(PeerRemoveParams {
            nickname: nickname.to_string(),
        }))
        .await
    }

    /// Rename a contact's nickname to `to` — every device sharing `user_id` when given, else the
    /// single provisional `nickname` entry — carrying its grants along. The daemon refuses (a
    /// [`ClientError::Api`]) when `to` is empty or already names a different identity. The daemon
    /// acks; the ack body is discarded.
    pub async fn peer_rename(
        &mut self,
        user_id: Option<String>,
        nickname: Option<String>,
        to: &str,
    ) -> Result<(), ClientError> {
        self.request_ack(Request::PeerRename(PeerRenameParams {
            user_id,
            nickname,
            to: to.to_string(),
        }))
        .await
    }

    /// Install a signed roster from the LOCAL file at `path` (`org_root_pk` pins the org root on
    /// FIRST install); return the installed org id + serial + severed-session count.
    pub async fn roster_install(
        &mut self,
        path: &str,
        org_root_pk: Option<String>,
    ) -> Result<RosterInstallResult, ClientError> {
        self.request_typed(
            Request::RosterInstall(RosterInstallParams {
                path: path.to_string(),
                org_root_pk,
            }),
            "roster_install result",
        )
        .await
    }

    /// Read the installed roster's MEMBERSHIP (#93): the declared groups, and every person with
    /// their display name, groups, and devices.
    ///
    /// Distinct from [`status`](Self::status)'s `presence`, which enumerates reachable DEVICES and
    /// omits a person entirely when none of theirs is up. This is the member list — everyone the
    /// roster carries, with `online` per device, so one read serves both questions.
    ///
    /// Advisory: display and authoring input, never an authorization answer. Empty in a
    /// pure-pairing daemon and before the first roster is installed. `api_minor >= 46`.
    pub async fn roster_members(
        &mut self,
    ) -> Result<crate::protocol::RosterMembersResult, ClientError> {
        self.request_typed(Request::RosterMembers, "roster_members result")
            .await
    }

    /// AUTHOR an org (#66): mint this node's org root key, sign an empty roster, install it (which
    /// pins the root), and return the copyable invite plus the root's fingerprint.
    ///
    /// **One-time per node** — a second call is refused rather than replacing the key, which would
    /// orphan every roster already signed with it.
    ///
    /// Show `org_root_fingerprint` to the operator: it is what every joiner reads back
    /// out-of-band, and it is the only thing anchoring their trust in the org. `api_minor >= 46`.
    pub async fn org_create(
        &mut self,
        name: &str,
        expires_secs: Option<i64>,
        roster_url: Option<String>,
    ) -> Result<crate::protocol::OrgCreateResult, ClientError> {
        self.request_typed(
            Request::OrgCreate(crate::protocol::OrgCreateParams {
                name: name.to_string(),
                expires_secs,
                roster_url,
            }),
            "org_create result",
        )
        .await
    }

    /// APPROVE a join code into the roster (#66): verify its device→user-key binding, add the
    /// member with `groups`, re-sign, install.
    ///
    /// **The result's `join_code_fingerprint` is not decoration.** Nothing in a join code binds it
    /// to a human, so a substituted code is caught by the two people comparing that fingerprint
    /// out-of-band, or it is not caught at all. Show it and have the operator confirm it.
    ///
    /// Each group must already be declared in the roster; an undeclared one is refused. `user_id`
    /// overrides the id the joiner requested — worth using, since that id is chosen by the person
    /// being approved and is what every `allow` entry will name. `api_minor >= 46`.
    pub async fn org_approve(
        &mut self,
        join_code: &str,
        groups: Vec<String>,
        user_id: Option<String>,
    ) -> Result<crate::protocol::OrgApproveResult, ClientError> {
        self.request_typed(
            Request::OrgApprove(crate::protocol::OrgApproveParams {
                join_code: join_code.to_string(),
                groups,
                user_id,
            }),
            "org_approve result",
        )
        .await
    }

    /// Rotate the org root (#93 ask c), publishing a bridge members adopt as they receive it.
    pub async fn org_rotate(
        &mut self,
        new_key_path: Option<String>,
    ) -> Result<crate::protocol::OrgRotateResult, ClientError> {
        self.request_typed(
            Request::OrgRotate(crate::protocol::OrgRotateParams { new_key_path }),
            "org_rotate result",
        )
        .await
    }

    /// Mint an attestation offer (#85 ask 3) — where another of this person's devices should dial.
    pub async fn attest_offer(
        &mut self,
    ) -> Result<crate::protocol::AttestOfferResult, ClientError> {
        self.request_typed(Request::AttestOffer, "attest_offer result")
            .await
    }

    /// Present this device's identity to a peer, using their `mcpmesh-attest:` line (#85 ask 3).
    pub async fn attest_to(
        &mut self,
        offer: impl Into<String>,
    ) -> Result<crate::protocol::PairResult, ClientError> {
        self.request_typed(
            Request::AttestTo(crate::protocol::AttestToParams {
                offer: offer.into(),
            }),
            "attest_to result",
        )
        .await
    }

    /// Refuse a peer's device on this node (#85 ask 4). Immediate: live sessions are severed.
    pub async fn peer_revoke(
        &mut self,
        peer: impl Into<String>,
        reason: Option<String>,
    ) -> Result<crate::protocol::PeerRevokeResult, ClientError> {
        self.request_typed(
            Request::PeerRevoke(crate::protocol::PeerRevokeParams {
                peer: peer.into(),
                reason,
            }),
            "peer_revoke result",
        )
        .await
    }

    /// Lift a local revocation (#85 ask 4). Idempotent.
    pub async fn peer_unrevoke(
        &mut self,
        peer: impl Into<String>,
    ) -> Result<crate::protocol::PeerUnrevokeResult, ClientError> {
        self.request_typed(
            Request::PeerUnrevoke(crate::protocol::PeerUnrevokeParams { peer: peer.into() }),
            "peer_unrevoke result",
        )
        .await
    }

    /// Sign a portable revocation of one of THIS person's own devices (#85 ask 4).
    pub async fn device_revoke(
        &mut self,
        endpoint: impl Into<String>,
        reason: Option<String>,
    ) -> Result<crate::protocol::DeviceRevokeResult, ClientError> {
        self.request_typed(
            Request::DeviceRevoke(crate::protocol::DeviceRevokeParams {
                endpoint: endpoint.into(),
                reason,
            }),
            "device_revoke result",
        )
        .await
    }

    /// Apply a peer's signed device revocation (#85 ask 4).
    pub async fn device_revocation_import(
        &mut self,
        token: impl Into<String>,
    ) -> Result<crate::protocol::DeviceRevocationImportResult, ClientError> {
        self.request_typed(
            Request::DeviceRevocationImport(crate::protocol::DeviceRevocationImportParams {
                token: token.into(),
            }),
            "device_revocation_import result",
        )
        .await
    }

    /// EXPORT this node's user key as a RECOVERY PHRASE (#85 ask 2).
    ///
    /// **The phrase is the private key**, in a form a person can write down. Anyone who reads it
    /// can present this identity. Show it once, to the person who owns it, and do not persist it
    /// anywhere you would not persist the key file. It is deliberately not logged or audited by the
    /// daemon; this response is the only place it exists.
    ///
    /// `user_id` is safe to display and record — compare it after an import to confirm the right
    /// identity came back. `api_minor >= 48`. Refused (`-32602`) on a device ENROLLED into another
    /// identity, whose local key is not the one it presents (`api_minor >= 63`, #219).
    pub async fn user_key_export(
        &mut self,
    ) -> Result<crate::protocol::UserKeyExportResult, ClientError> {
        self.request_typed(Request::UserKeyExport, "user_key_export result")
            .await
    }

    /// IMPORT a user key from a recovery phrase (#85 ask 2), so a person's `b64u:` survives the
    /// hardware.
    ///
    /// Refuses to overwrite an existing key unless `replace` is set: importing over a live key
    /// discards the identity this node presents, irreversibly without that key's own phrase.
    ///
    /// **Check the returned `user_id` against the one you are recovering.** The phrase's checksum
    /// catches most transcription errors, but the `user_id` is the definitive answer, and the only
    /// thing that distinguishes "restored the wrong key" from "my peers have not seen me yet".
    ///
    /// It does NOT get this device admitted by anyone: peers authorize per DEVICE, and a restored
    /// user key does not put this endpoint in anybody's allowlist. That is #85 ask 3, not shipped.
    /// `api_minor >= 48`. Without `replace` it refuses to overwrite a key an earlier import wrote,
    /// including within the same daemon lifetime (`api_minor >= 63`, #221).
    pub async fn user_key_import(
        &mut self,
        recovery_phrase: &str,
        replace: bool,
    ) -> Result<crate::protocol::UserKeyImportResult, ClientError> {
        self.request_typed(
            Request::UserKeyImport(crate::protocol::UserKeyImportParams {
                recovery_phrase: recovery_phrase.to_string(),
                replace,
            }),
            "user_key_import result",
        )
        .await
    }

    /// DETACH this device from an identity it was enrolled into (#214) — the inverse of
    /// `pair_opts(.., allow_self_enroll: true)`. Drops the adopted binding live and on disk; the
    /// node goes back to presenting its own identity (imported, else boot-derived), returned as
    /// `user_id`.
    ///
    /// Local only: peers who already learned this endpoint as that person's device are not told —
    /// that is `device_revoke`, from the device holding the key. Refused with `ERR_NOT_ENROLLED`
    /// when nothing is adopted. `api_minor >= 62`.
    pub async fn self_enroll_detach(
        &mut self,
    ) -> Result<crate::protocol::SelfEnrollDetachResult, ClientError> {
        self.request_typed(Request::SelfEnrollDetach, "self_enroll_detach result")
            .await
    }

    /// INSPECT a join code without approving it (#66): what it claims, and the fingerprint that
    /// decides whether to believe it. Read-only — nothing is signed or installed.
    ///
    /// **Call this before [`org_approve`](Self::org_approve), show
    /// `join_code_fingerprint`, and have the operator confirm it out-of-band.** Nothing in a join
    /// code binds it to a person; a substituted one carries a different key and diverges here. The
    /// fingerprint on the approval RESULT is the same words, but by then the member is in the
    /// signed roster — too late to decline.
    ///
    /// The claims (`display_name`, `requested_user_id`, `device_label`) are chosen by the sender.
    /// Render them; do not trust them. A forged binding is refused rather than described.
    /// `api_minor >= 46`.
    pub async fn org_join_code(
        &mut self,
        join_code: &str,
    ) -> Result<crate::protocol::OrgJoinCodeResult, ClientError> {
        self.request_typed(
            Request::OrgJoinCode(crate::protocol::OrgJoinCodeParams {
                join_code: join_code.to_string(),
            }),
            "org_join_code result",
        )
        .await
    }

    /// REVOKE from the roster (#66) — and sever the cut devices' live sessions, immediately.
    ///
    /// Three readings, and picking the wrong one is destructive, so the result reports which
    /// `mode` was applied: `"<user_id>/<label>"` cuts ONE device; a bare `user_id` removes the
    /// person and revokes ALL their devices; `user_key = true` is a key ROTATION — the person is
    /// removed but their devices stay un-revoked so the same hardware re-enrolls under a fresh
    /// user key. `api_minor >= 46`.
    pub async fn org_revoke(
        &mut self,
        target: &str,
        user_key: bool,
    ) -> Result<crate::protocol::OrgRevokeResult, ClientError> {
        self.request_typed(
            Request::OrgRevoke(crate::protocol::OrgRevokeParams {
                target: target.to_string(),
                user_key,
            }),
            "org_revoke result",
        )
        .await
    }

    /// Pin the org root on a JOINER (no roster yet). `user_key` is a LOCAL path — the key never
    /// crosses the API. Returns the pinned org id.
    pub async fn org_join(
        &mut self,
        org_id: &str,
        org_root_pk: &str,
        user_id: &str,
        user_key: &str,
    ) -> Result<OrgJoinResult, ClientError> {
        self.request_typed(
            Request::OrgJoin(OrgJoinParams {
                org_id: org_id.to_string(),
                org_root_pk: org_root_pk.to_string(),
                user_id: user_id.to_string(),
                user_key: user_key.to_string(),
            }),
            "org_join result",
        )
        .await
    }

    /// Pin the HTTPS roster URL (`[roster].url`) in the daemon's config. The daemon acks; the
    /// ack body is discarded.
    pub async fn set_roster_url(&mut self, url: &str) -> Result<(), ClientError> {
        self.request_ack(Request::SetRosterUrl(SetRosterUrlParams {
            url: url.to_string(),
        }))
        .await
    }

    /// Discover which services a paired `peer` (a nickname, `eid:`, or `b64u:`) CURRENTLY grants
    /// the caller (#52) — dials the peer and returns the service names its allow admits for the
    /// caller's principal (only your own admitted services, never the peer's full registry).
    pub async fn peer_services(&mut self, peer: &str) -> Result<Vec<String>, ClientError> {
        self.request_typed::<PeerServicesResult>(
            Request::PeerServices(PeerServicesParams {
                peer: peer.to_string(),
            }),
            "peer_services",
        )
        .await
        .map(|r| r.services)
    }

    /// Remove a service registration (#50) — the deregistration mirror of `register_service`.
    /// Removes the whole entry (allow included) + any ephemeral registration of the name, then
    /// hot-reloads. Idempotent: an unknown name is a clean no-op.
    pub async fn unregister_service(&mut self, name: &str) -> Result<(), ClientError> {
        self.request_ack(Request::UnregisterService(UnregisterServiceParams {
            name: name.to_string(),
        }))
        .await
    }

    /// Grant a stable `principal` (`b64u:`/`eid:`) access to `service` WITHOUT (re)pairing (#44)
    /// — the per-peer "sharing on" toggle. Idempotent; an unknown service is a clean no-op.
    pub async fn service_allow_grant(
        &mut self,
        service: &str,
        principal: &str,
    ) -> Result<(), ClientError> {
        self.request_ack(Request::ServiceAllowGrant(ServiceAllowParams {
            service: service.to_string(),
            principal: principal.to_string(),
        }))
        .await
    }

    /// Revoke a stable `principal` from `service`'s allow WITHOUT unpairing (#44) — the
    /// "sharing off" toggle. The peer's identity row is untouched; it just cannot open NEW
    /// sessions (in-flight ones run to completion). Idempotent.
    pub async fn service_allow_revoke(
        &mut self,
        service: &str,
        principal: &str,
    ) -> Result<(), ClientError> {
        self.request_ack(Request::ServiceAllowRevoke(ServiceAllowParams {
            service: service.to_string(),
            principal: principal.to_string(),
        }))
        .await
    }

    /// Set this node's opaque app-metadata blob (#39, roster mode): ≤256 bytes, folded
    /// signed into each presence heartbeat so paired peers read it in `status` presence —
    /// no per-peer session. `""` clears it; in-memory (re-set on startup).
    pub async fn set_app_metadata(&mut self, metadata: &str) -> Result<(), ClientError> {
        self.request_ack(Request::SetAppMetadata(SetAppMetadataParams {
            metadata: metadata.to_string(),
        }))
        .await
    }

    /// Set this node's CUSTOM relay set LIVE (#53). `relay_urls` is the desired set (each must
    /// parse as an iroh `RelayUrl`; empty is rejected). When the node is already in
    /// `relay_mode = "custom"`, the daemon diffs against the running endpoint and applies the
    /// delta live (iroh `insert_relay`/`remove_relay`) — no restart, no dropped sessions — then
    /// persists `[network]`. When the node is currently `default`/`disabled`, the config is
    /// persisted but the live mode transition isn't possible: the returned
    /// [`SetRelaysResult::restart_required`] is `true`. Idempotent (an unchanged set → `changed:
    /// false`, no writes).
    pub async fn set_relays(
        &mut self,
        relay_urls: &[String],
    ) -> Result<SetRelaysResult, ClientError> {
        self.request_typed::<SetRelaysResult>(
            Request::SetRelays(SetRelaysParams {
                relay_urls: relay_urls.to_vec(),
            }),
            "set_relays",
        )
        .await
    }

    /// Rename this node LIVE (#37): the daemon validates + persists `[identity].nickname`
    /// under its own config lock and updates the name future invites present — no restart.
    /// Peers keep their stored pairing-time nickname until a re-invite (display-only).
    pub async fn set_nickname(&mut self, nickname: &str) -> Result<(), ClientError> {
        self.request_ack(Request::SetNickname(SetNicknameParams {
            nickname: nickname.to_string(),
        }))
        .await
    }

    /// Summarize the daemon's LOCAL audit log into per-peer / per-service session counts
    /// (local-only — nothing is transmitted).
    pub async fn audit_summary(&mut self) -> Result<AuditSummaryResult, ClientError> {
        self.request_typed(Request::AuditSummary, "audit_summary result")
            .await
    }

    /// Publish a local file into `scope`; return the minted `mcpmesh/blob/1` ticket + hash.
    pub async fn blob_publish(
        &mut self,
        scope: &str,
        path: &str,
    ) -> Result<BlobPublishResult, ClientError> {
        self.request_typed(
            Request::BlobPublish(BlobPublishParams {
                scope: scope.to_string(),
                path: path.to_string(),
            }),
            "blob_publish result",
        )
        .await
    }

    /// List the daemon's blob scopes (name → hashes + grants + withdrawn).
    ///
    /// A DEFAULT LIMIT applies (#84b) — check `truncated` and page with
    /// [`blob_list_paged`](Self::blob_list_paged) rather than assuming you saw everything.
    pub async fn blob_list(&mut self) -> Result<BlobScopeList, ClientError> {
        self.blob_list_paged(Default::default()).await
    }

    /// List blob scopes with filters + paging (#84b, `api_minor >= 20`).
    pub async fn blob_list_paged(
        &mut self,
        params: crate::BlobListParams,
    ) -> Result<BlobScopeList, ClientError> {
        self.request_typed(Request::BlobList(params), "blob_list result")
            .await
    }

    /// Fetch a `mcpmesh/blob/1` ticket THROUGH the daemon (BLAKE3-verified), export to
    /// `dest_path`; return the verified hash + byte length.
    pub async fn blob_fetch(
        &mut self,
        ticket: &str,
        dest_path: &str,
    ) -> Result<BlobFetchResult, ClientError> {
        self.blob_fetch_from(ticket, dest_path, Vec::new()).await
    }

    /// [`blob_fetch`](Self::blob_fetch) with ADDITIONAL sources to try when the ticket's publisher
    /// does not answer (#83).
    ///
    /// Content addressing makes every recipient a potential source; a single-address ticket made
    /// that unusable, so a file shared with a room became unfetchable the moment the sender closed
    /// their laptop — even though others in the room already held the identical verified bytes.
    ///
    /// `from` takes stable principals (`eid:`, `b64u:`) or paired nicknames — the same vocabulary
    /// `open_session` takes, and naming a PERSON offers every device of theirs. They are tried in
    /// order, **after** the publisher, so a live publisher costs nothing and an offline one costs
    /// one dial timeout.
    ///
    /// **The bytes are BLAKE3-verified against the ticket's hash whoever serves them**, so an
    /// alternate cannot substitute content. It can refuse: an alternate serves only hashes it has
    /// republished into a scope that grants you (see `blob_republish`), and an ungranted one
    /// answers a permission error and the fetch moves on. Every failure mode falls through, not
    /// only an unreachable dial — a refusal, a missing hash, a reset, and a stalled transfer all
    /// move to the next source. `api_minor >= 47`.
    pub async fn blob_fetch_from(
        &mut self,
        ticket: &str,
        dest_path: &str,
        from: Vec<String>,
    ) -> Result<BlobFetchResult, ClientError> {
        self.request_typed(
            Request::BlobFetch(BlobFetchParams {
                ticket: ticket.to_string(),
                dest_path: dest_path.to_string(),
                from,
            }),
            "blob_fetch result",
        )
        .await
    }

    /// Stop every in-flight [`blob_fetch`](Self::blob_fetch) of `hash` (#172).
    ///
    /// **Send this on a DIFFERENT connection than the fetch it cancels.** This client is one
    /// request at a time — `&mut self` is borrowed until the fetch answers — so a cancel issued on
    /// the same client can only run after the thing it would cancel is already over. The cancelled
    /// fetch answers [`ERR_CANCELLED`](crate::ERR_CANCELLED) on its own connection.
    ///
    /// `cancelled: false` means nothing was fetching that blob here. That is the honest answer to a
    /// cancel that raced a fetch to completion, not an error.
    ///
    /// Needs `api_minor >= 44`; below it the method is unknown.
    pub async fn blob_fetch_cancel(
        &mut self,
        hash: &str,
    ) -> Result<BlobFetchCancelResult, ClientError> {
        self.request_typed(
            Request::BlobFetchCancel(BlobFetchCancelParams {
                hash: hash.to_string(),
            }),
            "blob_fetch_cancel result",
        )
        .await
    }

    /// Grant a scope to a principal — any flat-namespace entry: a group name, a user_id,
    /// or a nickname (the shared `principal_set` expansion).
    /// The daemon acks; the ack body is discarded (a JSON-RPC error surfaces as
    /// `ClientError::Api`). Granting a scope to your own user_id reaches ALL of that
    /// person's devices.
    pub async fn blob_grant(&mut self, scope: &str, principal: &str) -> Result<(), ClientError> {
        self.request_ack(Request::BlobGrant(BlobGrantParams {
            scope: scope.to_string(),
            principal: principal.to_string(),
        }))
        .await
    }

    /// The TYPED `subscribe` upgrade: send [`Request::Subscribe`] (after which the connection
    /// stops being request/response — see [`open_stream`](Self::open_stream)) and return a
    /// [`StreamSubscription`] yielding [`StreamFrame`]s. For raw frames (e.g. to tolerate frame
    /// types newer than this crate), use `open_stream("subscribe")` instead.
    pub async fn subscribe(self) -> Result<StreamSubscription, ClientError> {
        let (reader, writer) = self.open_stream("subscribe").await?;
        Ok(StreamSubscription {
            reader,
            _writer: writer,
        })
    }
}

/// A live [`Request::Subscribe`] stream yielding typed [`StreamFrame`]s (snapshot, then
/// events/lagged notices) until the daemon side closes. Holds the connection's write half for its
/// lifetime — a subscriber only reads, but dropping the writer would half-close the socket. Drop
/// the subscription to disconnect (there is no request channel back).
pub struct StreamSubscription {
    reader: FrameReader<ControlRead>,
    _writer: ControlWrite,
}

/// Hand-rolled like [`ControlClient`]'s: the boxed transport halves are not `Debug`.
impl std::fmt::Debug for StreamSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamSubscription").finish_non_exhaustive()
    }
}

impl StreamSubscription {
    /// The next frame, or `None` when the daemon closed the stream. A frame this crate's
    /// [`StreamFrame`] does not model (a NEWER daemon's frame type) surfaces as
    /// [`ClientError::Malformed`] — a forward-compatible consumer reads raw frames via
    /// [`ControlClient::open_stream`] instead.
    pub async fn next(&mut self) -> Result<Option<StreamFrame>, ClientError> {
        match self.reader.next().await? {
            Some(Inbound::Frame(v)) => serde_json::from_value(v)
                .map(Some)
                .map_err(|_| ClientError::Malformed("stream frame")),
            Some(Inbound::Violation(_)) => Err(ClientError::Malformed("stream frame")),
            None => Ok(None),
        }
    }
}

/// Complete the mcpmesh-local/1 hello handshake over ALREADY-CONNECTED byte halves —
/// the transport-agnostic core of [`connect_control`], and the front door for in-process
/// embedding (`mcpmesh-node`'s `Node::control` dials a tokio duplex through here).
pub async fn connect_control_io(
    reader: impl tokio::io::AsyncRead + Send + Unpin + 'static,
    writer: impl tokio::io::AsyncWrite + Send + Unpin + 'static,
) -> Result<ControlClient, ClientError> {
    let mut reader = FrameReader::new(Box::new(reader) as ControlRead, MAX_FRAME_BYTES);
    let hello: Hello = match reader.next().await? {
        Some(Inbound::Frame(v)) => {
            serde_json::from_value(v).map_err(|_| ClientError::Malformed("hello"))?
        }
        Some(Inbound::Violation(_)) => return Err(ClientError::Malformed("hello")),
        None => return Err(ClientError::Closed("hello")),
    };
    if hello.api != crate::protocol::API_NAME {
        return Err(ClientError::WrongApi {
            got: hello.api,
            want: crate::protocol::API_NAME,
        });
    }
    Ok(ControlClient {
        hello,
        reader,
        writer: Box::new(writer) as ControlWrite,
    })
}

/// Connect + complete the hello handshake, asserting the api name is `mcpmesh-local/1`.
pub async fn connect_control(path: &Path) -> Result<ControlClient, ClientError> {
    let stream = connect_local(path).await?;
    let (read_half, write_half) = split_local(stream);
    connect_control_io(read_half, write_half).await
}

/// [`connect_control`] at the platform default endpoint ([`crate::paths::default_endpoint`]):
/// the quickstart front door — a consumer dials the running daemon without reimplementing
/// the platform endpoint rule. Resolution failure surfaces as [`ClientError::Io`]
/// (`NotFound`), same as a daemon that is not running.
pub async fn connect_control_default() -> Result<ControlClient, ClientError> {
    connect_control(&crate::paths::default_endpoint()?).await
}

// Seam-ported (Task 6): every stub daemon binds via the platform seam
// (`transport::bind_local` + `LocalListener::accept`) rather than a raw `UnixListener`,
// so these exercise the platform-identical `ControlClient` on BOTH unix (UDS) and windows
// (named pipe). Gated on `feature = "service"` (bind needs it) rather than `unix`: under
// `cargo test --workspace` feature unification turns `service` on for this crate (cli
// depends on local-api with features=["service"]), so the module compiles and RUNS on the
// windows CI leg. `test_endpoint` yields a platform-appropriate unique endpoint.
#[cfg(all(test, feature = "service"))]
mod tests {
    use super::*;
    use crate::protocol::{API_NAME, API_VERSION, BackendKind, ServiceInfo, StatusResult};
    use crate::transport::{LocalListener, bind_local, split_local};
    use tokio::io::AsyncWriteExt;

    /// A unique local endpoint for a stub daemon, platform-appropriate: a tempdir socket
    /// path on unix, a per-process-unique `\\.\pipe\…` name on windows. Returns the
    /// endpoint plus a guard that MUST outlive the listener (the `TempDir` on unix; unit
    /// on windows, whose pipe namespace needs no filesystem cleanup).
    #[cfg(unix)]
    fn test_endpoint(tag: &str) -> (std::path::PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{tag}.sock"));
        (path, dir)
    }
    #[cfg(windows)]
    fn test_endpoint(tag: &str) -> (std::path::PathBuf, ()) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::path::PathBuf::from(format!(
            r"\\.\pipe\mcpmesh-client-test-{}-{tag}-{n}",
            std::process::id()
        ));
        (path, ())
    }

    /// A stub mcpmesh daemon: send Hello, then answer one `status` with a StatusResult.
    async fn stub_daemon(mut listener: LocalListener) {
        let stream = listener.accept().await.unwrap();
        let (read_half, mut writer) = split_local(stream);
        write_frame(
            &mut writer,
            &serde_json::to_value(Hello {
                api: API_NAME.into(),
                api_version: API_VERSION.into(),
                api_minor: 0,
                stack_version: "0.1.0".into(),
            })
            .unwrap(),
        )
        .await
        .unwrap();
        let mut reader = FrameReader::new(read_half, MAX_FRAME_BYTES);
        let req = match reader.next().await.unwrap().unwrap() {
            Inbound::Frame(v) => v,
            Inbound::Violation(_) => panic!("violation"),
        };
        assert_eq!(req["method"], "status");
        let result = StatusResult {
            stack_version: "0.1.0".into(),
            services: vec![ServiceInfo {
                name: "kb".into(),
                allow: vec![],
                allow_display: vec![],
                backend: BackendKind::Socket,
                ephemeral: false,
            }],
            peers: vec![],
            roster: None,
            presence: vec![],
            self_user_id: None,
            self_user_key_held: false,
            recent_pairings: vec![],
            reachability: vec![],
            self_nickname: String::new(),
            storage: None,
            revoked: Vec::new(),
            self_network: None,
        };
        write_frame(
            &mut writer,
            &serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": result }),
        )
        .await
        .unwrap();
        writer.flush().await.unwrap();
    }

    /// The transport-agnostic front door: the same hello handshake over a plain in-memory
    /// duplex — what an embedded node's `Node::control` dials through.
    #[tokio::test]
    async fn connect_control_io_handshakes_over_a_duplex() {
        let (client_io, mut server_io) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            write_frame(
                &mut server_io,
                &serde_json::to_value(Hello {
                    api: API_NAME.into(),
                    api_version: API_VERSION.into(),
                    api_minor: 0,
                    stack_version: "in-proc".into(),
                })
                .unwrap(),
            )
            .await
            .unwrap();
        });
        let (r, w) = tokio::io::split(client_io);
        let client = connect_control_io(r, w).await.expect("handshake");
        assert_eq!(client.hello().stack_version, "in-proc");
    }

    #[tokio::test]
    async fn connect_reads_hello_asserts_api_and_requests() {
        let (sock, _guard) = test_endpoint("status");
        let listener = bind_local(&sock).unwrap();
        let server = tokio::spawn(stub_daemon(listener));

        let mut client = connect_control(&sock).await.unwrap();
        assert_eq!(client.hello().api, API_NAME);
        let result = client.request(Request::Status).await.unwrap();
        assert_eq!(result["services"][0]["name"], "kb");
        assert_eq!(result["services"][0]["backend"], "socket");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn wrong_api_hello_is_rejected() {
        let (sock, _guard) = test_endpoint("wrongapi");
        let listener = bind_local(&sock).unwrap();
        tokio::spawn(async move {
            let mut listener = listener;
            let stream = listener.accept().await.unwrap();
            let (_r, mut w) = split_local(stream);
            write_frame(
                &mut w,
                &serde_json::json!({"api":"other/1","api_version":"1.0","stack_version":"0"}),
            )
            .await
            .unwrap();
            w.flush().await.unwrap();
        });
        match connect_control(&sock).await {
            Err(ClientError::WrongApi { got, want }) => {
                assert_eq!(got, "other/1");
                assert_eq!(want, API_NAME);
            }
            other => panic!("expected WrongApi, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn blob_fetch_and_publish_deserialize_typed_results() {
        use crate::protocol::{BlobFetchResult, BlobPublishResult};
        let (sock, _guard) = test_endpoint("blob");
        let listener = bind_local(&sock).unwrap();
        let server = tokio::spawn(async move {
            let mut listener = listener;
            let stream = listener.accept().await.unwrap();
            let (read_half, mut writer) = split_local(stream);
            write_frame(
                &mut writer,
                &serde_json::to_value(Hello {
                    api: API_NAME.into(),
                    api_version: API_VERSION.into(),
                    api_minor: 0,
                    stack_version: "0.1.0".into(),
                })
                .unwrap(),
            )
            .await
            .unwrap();
            let mut reader = FrameReader::new(read_half, MAX_FRAME_BYTES);
            // First request: blob_publish -> a ticket + hash.
            let req = match reader.next().await.unwrap().unwrap() {
                Inbound::Frame(v) => v,
                Inbound::Violation(_) => panic!("violation"),
            };
            assert_eq!(req["method"], "blob_publish");
            assert_eq!(req["params"]["scope"], "eng");
            write_frame(
                &mut writer,
                &serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"ticket":"blobT","hash":"ab"}}),
            )
            .await
            .unwrap();
            // Second request: blob_fetch -> a verified hash + length.
            let req = match reader.next().await.unwrap().unwrap() {
                Inbound::Frame(v) => v,
                Inbound::Violation(_) => panic!("violation"),
            };
            assert_eq!(req["method"], "blob_fetch");
            assert_eq!(req["params"]["ticket"], "blobT");
            assert_eq!(req["params"]["dest_path"], "/tmp/out.bin");
            write_frame(
                &mut writer,
                &serde_json::json!({"jsonrpc":"2.0","id":2,"result":{"hash":"cd","bytes_len":7}}),
            )
            .await
            .unwrap();
            let _ = (
                BlobFetchResult {
                    hash: "cd".into(),
                    bytes_len: 7,
                },
                BlobPublishResult {
                    ticket: "blobT".into(),
                    hash: "ab".into(),
                },
            );
        });

        let mut client = connect_control(&sock).await.unwrap();
        let pub_res = client.blob_publish("eng", "/tmp/a.bin").await.unwrap();
        assert_eq!(pub_res.ticket, "blobT");
        assert_eq!(pub_res.hash, "ab");
        let fetch_res = client.blob_fetch("blobT", "/tmp/out.bin").await.unwrap();
        assert_eq!(fetch_res.hash, "cd");
        assert_eq!(fetch_res.bytes_len, 7);
        server.await.unwrap();
    }

    /// Regression (lossless rebox): a frame the server PIPELINES in the same write as
    /// the Hello must survive `open_session` + kb's production re-box shape
    /// (`FrameReader::new(Box::new(reader.into_inner()), …)`, bridge/session.rs). Against
    /// the old `into_inner -> R` — which unwrapped the internal `BufReader` and DROPPED
    /// its read-ahead — the pipelined frame vanished and this test failed (EOF instead of
    /// the frame). `into_inner -> BufReader<R>` carries the read-ahead across the rebox.
    #[tokio::test]
    async fn frame_pipelined_behind_hello_survives_open_session_rebox() {
        use tokio::io::AsyncRead;

        let (sock, _guard) = test_endpoint("pipelined");
        let listener = bind_local(&sock).unwrap();
        let server = tokio::spawn(async move {
            let mut listener = listener;
            let stream = listener.accept().await.unwrap();
            let (read_half, mut writer) = split_local(stream);
            // ONE write carrying the Hello AND a session frame → both land in the
            // client's first BufReader fill (the read-ahead under test).
            let mut bytes = serde_json::to_vec(
                &serde_json::to_value(Hello {
                    api: API_NAME.into(),
                    api_version: API_VERSION.into(),
                    api_minor: 0,
                    stack_version: "0.1.0".into(),
                })
                .unwrap(),
            )
            .unwrap();
            bytes.push(b'\n');
            bytes.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":42,\"result\":{}}\n");
            writer.write_all(&bytes).await.unwrap();
            writer.flush().await.unwrap();
            // Absorb the client's open_session frame so its write never sees EPIPE.
            let mut reader = FrameReader::new(read_half, MAX_FRAME_BYTES);
            let req = match reader.next().await.unwrap().unwrap() {
                Inbound::Frame(v) => v,
                Inbound::Violation(_) => panic!("violation"),
            };
            assert_eq!(req["method"], "open_session");
        });

        let client = connect_control(&sock).await.unwrap();
        let (reader, _writer) = client
            .open_session("peer".into(), "kb".into())
            .await
            .unwrap();
        // kb's production shape: erase the half type behind a boxed pipe, then re-frame.
        let boxed: Box<dyn AsyncRead + Unpin + Send> = Box::new(reader.into_inner());
        let mut reframed = FrameReader::new(boxed, MAX_FRAME_BYTES);
        match reframed.next().await.unwrap() {
            Some(Inbound::Frame(v)) => assert_eq!(v["id"], 42),
            other => panic!("pipelined frame was lost across the rebox: {other:?}"),
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn blob_grant_issues_request_and_acks() {
        let (sock, _guard) = test_endpoint("grant");
        let listener = bind_local(&sock).unwrap();
        let server = tokio::spawn(async move {
            let mut listener = listener;
            let stream = listener.accept().await.unwrap();
            let (read_half, mut writer) = split_local(stream);
            write_frame(
                &mut writer,
                &serde_json::to_value(Hello {
                    api: API_NAME.into(),
                    api_version: API_VERSION.into(),
                    api_minor: 0,
                    stack_version: "0.1.0".into(),
                })
                .unwrap(),
            )
            .await
            .unwrap();
            let mut reader = FrameReader::new(read_half, MAX_FRAME_BYTES);
            let req = match reader.next().await.unwrap().unwrap() {
                Inbound::Frame(v) => v,
                Inbound::Violation(_) => panic!("violation"),
            };
            assert_eq!(req["method"], "blob_grant");
            assert_eq!(req["params"]["scope"], "kb-sync");
            assert_eq!(req["params"]["principal"], "alice");
            write_frame(
                &mut writer,
                &serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}}),
            )
            .await
            .unwrap();
        });
        let mut client = connect_control(&sock).await.unwrap();
        client.blob_grant("kb-sync", "alice").await.unwrap();
        server.await.unwrap();
    }

    /// The typed `status()` helper pairs `Request::Status` with `StatusResult` — the caller gets
    /// the struct, not a `Value` to hand-deserialize (and a malformed result surfaces as
    /// `ClientError::Malformed`, never a silently-wrong type).
    #[tokio::test]
    async fn typed_status_helper_deserializes_the_result() {
        let (sock, _guard) = test_endpoint("typedstatus");
        let listener = bind_local(&sock).unwrap();
        let server = tokio::spawn(stub_daemon(listener));

        let mut client = connect_control(&sock).await.unwrap();
        let status = client.status().await.unwrap();
        assert_eq!(status.stack_version, "0.1.0");
        assert_eq!(status.services[0].name, "kb");
        assert_eq!(status.services[0].backend, BackendKind::Socket);
        assert!(status.peers.is_empty());
        server.await.unwrap();
    }

    /// The ack-shaped typed helpers issue the right wire method and discard the `{}` ack; a
    /// JSON-RPC error frame surfaces as `ClientError::Api`.
    #[tokio::test]
    async fn typed_ack_helpers_issue_requests_and_surface_api_errors() {
        let (sock, _guard) = test_endpoint("typedack");
        let listener = bind_local(&sock).unwrap();
        let server = tokio::spawn(async move {
            let mut listener = listener;
            let stream = listener.accept().await.unwrap();
            let (read_half, mut writer) = split_local(stream);
            write_frame(
                &mut writer,
                &serde_json::to_value(Hello {
                    api: API_NAME.into(),
                    api_version: API_VERSION.into(),
                    api_minor: 0,
                    stack_version: "0.1.0".into(),
                })
                .unwrap(),
            )
            .await
            .unwrap();
            let mut reader = FrameReader::new(read_half, MAX_FRAME_BYTES);
            // peer_remove → ack.
            let req = match reader.next().await.unwrap().unwrap() {
                Inbound::Frame(v) => v,
                Inbound::Violation(_) => panic!("violation"),
            };
            assert_eq!(req["method"], "peer_remove");
            assert_eq!(req["params"]["nickname"], "bob");
            write_frame(
                &mut writer,
                &serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}}),
            )
            .await
            .unwrap();
            // peer_rename → an error frame (collision refusal).
            let req = match reader.next().await.unwrap().unwrap() {
                Inbound::Frame(v) => v,
                Inbound::Violation(_) => panic!("violation"),
            };
            assert_eq!(req["method"], "peer_rename");
            assert_eq!(req["params"]["to"], "Bobby");
            write_frame(
                &mut writer,
                &serde_json::json!({"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"taken"}}),
            )
            .await
            .unwrap();
        });

        let mut client = connect_control(&sock).await.unwrap();
        client.peer_remove("bob").await.unwrap();
        match client.peer_rename(None, Some("bob".into()), "Bobby").await {
            Err(ClientError::Api(e)) => assert_eq!(e["message"], "taken"),
            other => panic!("expected Api error, got {other:?}"),
        }
        server.await.unwrap();
    }

    /// The typed `subscribe()` upgrade yields `StreamFrame`s — snapshot, event, lagged — then
    /// `None` when the daemon side closes.
    #[tokio::test]
    async fn typed_subscribe_yields_frames_then_end() {
        use crate::protocol::{ActiveSession, AuditRecord, PeerReachability};

        let (sock, _guard) = test_endpoint("subscribe");
        let listener = bind_local(&sock).unwrap();
        let server = tokio::spawn(async move {
            let mut listener = listener;
            let stream = listener.accept().await.unwrap();
            let (read_half, mut writer) = split_local(stream);
            write_frame(
                &mut writer,
                &serde_json::to_value(Hello {
                    api: API_NAME.into(),
                    api_version: API_VERSION.into(),
                    api_minor: 0,
                    stack_version: "0.1.0".into(),
                })
                .unwrap(),
            )
            .await
            .unwrap();
            let mut reader = FrameReader::new(read_half, MAX_FRAME_BYTES);
            let req = match reader.next().await.unwrap().unwrap() {
                Inbound::Frame(v) => v,
                Inbound::Violation(_) => panic!("violation"),
            };
            assert_eq!(req["method"], "subscribe");
            for frame in [
                StreamFrame::Snapshot {
                    self_network: None,
                    active_sessions: vec![ActiveSession {
                        peer: "bob".into(),
                        service: "notes".into(),
                        opened_at: 7,
                        principal: Some("eid:bob".into()),
                    }],
                    reachability: vec![PeerReachability {
                        name: "bob".into(),
                        reachable: true,
                        rtt_ms: Some(42),
                        age_secs: Some(3),
                        meta: String::new(),
                        principal: None,
                        path: Default::default(),
                    }],
                },
                StreamFrame::Event {
                    record: Box::new(AuditRecord::session_open(
                        "2026-07-03T14:02:11.480Z".into(),
                        Some("bob".into()),
                        "notes".into(),
                        None,
                    )),
                },
                StreamFrame::Lagged { dropped: 12 },
            ] {
                write_frame(&mut writer, &serde_json::to_value(&frame).unwrap())
                    .await
                    .unwrap();
            }
            writer.flush().await.unwrap();
            // Drop the connection: the client must see the stream END (Ok(None)), not an error.
        });

        let client = connect_control(&sock).await.unwrap();
        let mut sub = client.subscribe().await.unwrap();
        match sub.next().await.unwrap().unwrap() {
            StreamFrame::Snapshot {
                active_sessions,
                reachability,
                ..
            } => {
                assert_eq!(active_sessions[0].peer, "bob");
                assert_eq!(reachability[0].rtt_ms, Some(42));
            }
            other => panic!("expected the snapshot first, got {other:?}"),
        }
        match sub.next().await.unwrap().unwrap() {
            StreamFrame::Event { record } => {
                assert_eq!(record.peer.as_deref(), Some("bob"));
                assert_eq!(record.service.as_deref(), Some("notes"));
            }
            other => panic!("expected the event, got {other:?}"),
        }
        assert_eq!(
            sub.next().await.unwrap(),
            Some(StreamFrame::Lagged { dropped: 12 })
        );
        assert_eq!(sub.next().await.unwrap(), None, "clean end of stream");
        server.await.unwrap();
    }
}
