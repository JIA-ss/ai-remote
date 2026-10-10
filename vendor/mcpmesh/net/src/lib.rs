//! mcpmesh-net: the session kernel of the mcpmesh workspace — endpoint identity,
//! framed transports, trust-gated serving, and connection severing over iroh/QUIC.
//!
//! This crate is shared by the mcpmesh daemon (the serving side) and anything that
//! dials mesh services with full identity control. It deliberately excludes trust
//! POLICY (rosters, pairing, key storage — `mcpmesh-trust` and the daemon own
//! those; this crate only defines the [`TrustGate`] trait they implement) and the
//! wire codec's implementation (owned by `mcpmesh-codec`, re-exported here as
//! [`framing`]). Most integrators talking to a locally-running mesh want
//! `mcpmesh-local-api` instead — it needs no iroh at all.
//!
//! # The iroh pin
//!
//! This crate exact-pins its iroh version and exposes iroh types throughout its
//! public API (`iroh::Endpoint`, `iroh::EndpointAddr`, the stream types inside
//! [`SessionTransport`]). Use the re-export — `mcpmesh_net::iroh::…` — and never
//! add your own `iroh` dependency: any other version is a different crate to the
//! type system, and the first floating requirement breaks the build.
//!
//! # The rmcp pin — and why it is currently a PRERELEASE
//!
//! [`NdjsonTransport`] implements `rmcp::transport::Transport`, so rmcp is in this crate's PUBLIC
//! API exactly as iroh is. The same rule applies, and right now it applies harder: the pin is an
//! exact **prerelease** (`=3.0.0-beta.3`), and Cargo does not match a caret requirement against a
//! prerelease. A downstream writing `rmcp = "3"` — or `rmcp = "2"`, which worked before — gets TWO
//! rmcp crates in the graph, and our `Transport` impl is on a different trait than theirs. It
//! compiles, then fails at the use site with `expected Transport, found Transport`, no version in
//! the message.
//!
//! Use the re-export — `mcpmesh_net::rmcp::…` — and never add your own `rmcp` dependency.
//!
//! We track the SDK ahead of the coming MCP spec change: it already knows
//! `ProtocolVersion::V_2026_07_28` while still defaulting to `V_2025_11_25`, so the constant is
//! available before we opt into the stateless revision (#45). This re-export is what keeps that
//! choice from being a hard break for embedders.
//!
//! The pin is on a STABLE release as of `=3.1.0` (#46) — it was a `3.0.0-beta` while the stateless
//! revision was still settling. `LATEST` is the same `V_2025_11_25` in both, so moving off the
//! prerelease changed nothing about what we negotiate.
pub use iroh;
pub use rmcp;

pub mod endpoint;
pub mod errors;
pub mod framing;
pub mod identity;
pub mod registry;
pub mod service;
pub mod transport;
pub use endpoint::{
    ALPN_MCP, ALPN_PAIR, ALPN_PING, CLOSE_UNAUTHORIZED, ConnectError, LiveServices, ServeHandle,
    ServiceEntry, ServiceKind, Services, SessionBackend, SessionTransport, connect,
    connect_with_transport_config, run_mesh_connection, serve,
};
pub use framing::{
    FrameReader, Inbound, MAX_FRAME_BYTES, StrikeOutcome, Strikes, Violation, write_frame,
};
pub use identity::{EndpointId, PeerIdentity, StaticGate, TrustGate};
pub use registry::{ConnRegistry, Registration, should_sever};
pub use transport::{NdjsonTransport, RecvError, TransportWriter};
