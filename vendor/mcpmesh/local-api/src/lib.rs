//! Talk to a running [mcpmesh](https://github.com/counterpunchtech/mcpmesh) daemon from Rust.
//!
//! This crate is the `mcpmesh-local/1` control seam: the typed wire vocabulary of the daemon's
//! local control endpoint (requests, results, and the live-stream frames), the platform rule for
//! finding that endpoint ([`paths`]), and — behind the `client` feature — an async client that
//! speaks it. It links **no networking stack**: embedders (UIs, plugins, scripts) drive the
//! daemon without pulling the mesh transport.
//!
//! The full protocol (framing, method-by-method semantics, the identity contract) is documented in
//! [`docs/local-protocol.md`](https://github.com/counterpunchtech/mcpmesh/blob/main/docs/local-protocol.md).
//! To RUN a node in-process instead of driving a daemon, see
//! [`mcpmesh-node`](https://docs.rs/mcpmesh-node) — its `Node::control()` returns this same
//! [`ControlClient`] over an in-memory pipe.
//!
//! # Quickstart (feature `client`)
//!
#![cfg_attr(feature = "client", doc = "```no_run")]
#![cfg_attr(not(feature = "client"), doc = "```ignore")]
//! # async fn quickstart() -> Result<(), mcpmesh_local_api::client::ClientError> {
//! let mut daemon = mcpmesh_local_api::connect_control_default().await?;
//! let status = daemon.status().await?;
//! for peer in &status.peers {
//!     println!("{} shares: {}", peer.name, peer.services.join(", "));
//! }
//! # Ok(())
//! # }
//! ```
//!
//! [`connect_control_default`] dials the platform default endpoint (a unix socket, or a named
//! pipe on Windows — the one rule in [`paths::default_endpoint`]); [`ControlClient`] then offers
//! a typed helper per control method (`status`, `invite`, `pair`, `subscribe`, …), with
//! [`ControlClient::request`] as the raw escape hatch for forward compatibility.
//!
//! # Features
//!
//! | Feature   | Adds                                                                    | Dependencies |
//! |-----------|-------------------------------------------------------------------------|--------------|
//! | *(none)*  | The wire vocabulary ([`protocol`]) + endpoint resolution ([`paths`])    | serde only   |
//! | `client`  | [`ControlClient`]: connect, typed request helpers, the typed [`client::StreamSubscription`] live stream | + tokio |
//! | `service` | The plugin seam ([`service`]): local endpoint bind + same-user gate, `[services.*]` self-registration | + rustix, tracing |

/// This crate's version — the mcpmesh release train the `mcpmesh` binary ships on.
/// Embedders that bundle the daemon binary pin both to ONE version and use this const
/// as the expected `stack_version` anchor for the daemon they spawned.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The canonical transport-vocabulary blocklist (spec §1.5/§17), shipped in-crate so
/// embedders' surface-leak suites assert against the ONE canonical copy instead of
/// forking it. JSON object with `substring_banned`, `token_banned`, `carve_outs`.
pub const TRANSPORT_VOCABULARY: &str = include_str!("../fixtures/transport-vocabulary.json");

/// Platform paths + endpoint resolution — featureless/std-only, so any consumer
/// resolves the daemon endpoint from the ONE rule.
pub mod paths;
pub mod principals;
pub mod protocol;
pub use principals::principal_set;
pub use protocol::{
    API_MINOR, API_NAME, API_VERSION, ActiveSession, AttestOfferResult, AttestToParams, AuditKind,
    AuditListParams, AuditListResult, AuditPruneParams, AuditPruneResult, AuditRecord,
    AuditSummaryResult, BackendKind, BackendSpec, BlobDirection, BlobFetchCancelParams,
    BlobFetchCancelResult, BlobFetchParams, BlobFetchResult, BlobGrantParams, BlobListParams,
    BlobPublishParams, BlobPublishResult, BlobRepublishParams, BlobRevokeParams, BlobScopeList,
    BlobTransferState, BlobUnpublishParams, BlobsGcInfo, DeviceRevocationImportParams,
    DeviceRevocationImportResult, DeviceRevokeParams, DeviceRevokeResult, ERR_BLOB_WITHDRAWN,
    ERR_CANCELLED, ERR_INVITE_EXPIRED, ERR_INVITE_NAME_CONFLICT, ERR_INVITE_NOT_LIVE,
    ERR_INVITE_REFUSED, ERR_INVITER_MISMATCH, ERR_INVITER_UNREACHABLE, ERR_NICKNAME_TAKEN,
    ERR_NO_SUCH_BLOB, ERR_NO_SUCH_SERVICE, ERR_NOT_ENROLLED, ERR_PAIR_IDENTITY_REVOKED,
    ERR_PRINCIPAL_REVOKED, ERR_SELF_ENROLL_NO_KEY, ERR_SELF_ENROLL_NOT_OFFERED,
    ERR_TOO_MANY_INFLIGHT, Hello, InviteParams, InviteResult, KnownAddr, MAX_BLOB_SOURCES,
    MAX_INFLIGHT, MAX_INVITE_USES, OpenSessionParams, OrgApproveParams, OrgApproveResult,
    OrgCreateParams, OrgCreateResult, OrgJoinCodeParams, OrgJoinCodeResult, OrgJoinParams,
    OrgJoinResult, OrgRevokeParams, OrgRevokeResult, OrgRotateParams, OrgRotateResult, PairParams,
    PairResult, PeerAddParams, PeerDiagnosticsParams, PeerDiagnosticsResult, PeerEndorseParams,
    PeerEndorseResult, PeerHintClearParams, PeerHintClearResult, PeerInfo, PeerIntroduceParams,
    PeerPath, PeerReachability, PeerRemoveParams, PeerRenameParams, PeerRevokeParams,
    PeerRevokeResult, PeerServicesParams, PeerServicesResult, PeerUnrevokeParams,
    PeerUnrevokeResult, PresencePeer, ReachabilitySource, RecentPairing, RegisterServiceParams,
    RelayInfo, Request, RevokedEndpoint, RosterInstallParams, RosterInstallResult, RosterMember,
    RosterMemberDevice, RosterMembersResult, RosterStatus, ScopeInfo, SelfEnrollDetachResult,
    SelfNetwork, ServiceAllowParams, ServiceInfo, SetAppMetadataParams, SetNicknameParams,
    SetRelaysParams, SetRelaysResult, SetRosterUrlParams, StatusResult, StorageInfo, StreamFrame,
    UnregisterServiceParams, UserKeyExportResult, UserKeyImportParams, UserKeyImportResult,
    method_of, one_use,
};

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub mod codec;

/// The platform local-endpoint seam: connect/bind/accept/authorize.
#[cfg(feature = "client")]
pub mod transport;
#[cfg(feature = "client")]
pub use client::{
    ControlClient, ControlRead, ControlWrite, StreamSubscription, connect_control,
    connect_control_default, connect_control_io,
};

/// The shared plugin-platform seam (kb, loc, …): local endpoint faces, THE audience-authz
/// expansion, `[services.*]` self-registration, and the `*-local/1` JSON-RPC conventions.
#[cfg(feature = "service")]
pub mod service;
