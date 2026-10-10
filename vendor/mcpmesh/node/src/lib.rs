//! Embed a full [mcpmesh](https://github.com/counterpunchtech/mcpmesh) node in-process.
//!
//! The supported surface is [`NodeBuilder`]/[`Node`] plus the `mcpmesh-local/1` control
//! protocol [`Node::control`] speaks (see `docs/local-protocol.md`). Every other module in
//! this crate is `#[doc(hidden)]` internals of the mcpmesh daemon — no stability promise is
//! made for them; they may change or vanish in any release without a major version bump.
//!
//! Only driving a RUNNING daemon (the sidecar model)? Depend on
//! [`mcpmesh-local-api`](https://docs.rs/mcpmesh-local-api) instead — it links no
//! networking stack at all. The full embedding guide (root-dir layout, host contract,
//! sidecar-vs-embedded) is
//! [`docs/embedding.md`](https://github.com/counterpunchtech/mcpmesh/blob/main/docs/embedding.md).
//!
//! # Quickstart
//!
//! ```no_run
//! # async fn quickstart() -> anyhow::Result<()> {
//! let node = mcpmesh_node::NodeBuilder::new("/var/lib/myapp/mesh").start().await?;
//! let mut control = node.control().await?;
//! control.register_service(
//!     "notes",
//!     mcpmesh_local_api::BackendSpec::Run {
//!         cmd: vec!["my-mcp-server".into()],
//!         env: Default::default(),
//!         cwd: None,
//!     },
//!     vec![],
//! ).await?;
//! let invite = control.invite(vec!["notes".into()]).await?;
//! println!("send this to a friend: {}", invite.invite_line);
//! # Ok(())
//! # }
//! ```
//!
//! The node is a full peer: everything the mcpmesh daemon can do — pairing, live MCP
//! sessions (`open_session`), roster/org mode, blobs, audit — works through
//! [`Node::control`], because it runs the daemon's own handlers.

/// This crate's version — the mcpmesh release-train version the daemon binary ships on.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use config::Config;
/// The pinned iroh, re-exported (#67).
///
/// [`Node::accept_protocol`](node::Node::accept_protocol),
/// [`Node::connect_protocol`](node::Node::connect_protocol) and
/// [`Node::connection_path`](node::Node::connection_path) name iroh types in their signatures,
/// so an embedder needs them in scope. Use THIS path — never your own `iroh` dependency: a
/// version mismatch is a different crate to the type system and breaks the build with an
/// `expected Connection, found Connection`.
pub use mcpmesh_net::iroh;
/// The pinned `mcpmesh-trust`, re-exported (#85).
///
/// [`NodeBuilder::device_key`](node::NodeBuilder::device_key) takes a
/// `mcpmesh_trust::ed25519_dalek::SigningKey`, so an embedder needs that type in scope. Use THIS
/// path rather than adding your own `mcpmesh-trust` or `ed25519-dalek` dependency — the version
/// hazard is identical to the iroh one above, and the compiler error is just as unhelpful.
pub use mcpmesh_trust;
pub use node::{Node, NodeBuilder, StartError};
pub use paths::NodePaths;

#[doc(hidden)]
pub mod allowlist;
#[doc(hidden)]
pub mod audit;
#[doc(hidden)]
pub mod backends;
#[doc(hidden)]
pub mod blobs;
#[doc(hidden)]
pub mod cancel;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod control;
#[doc(hidden)]
pub mod daemon;
/// Node-level conditions that are observable but have no API to observe them through (#134) —
/// currently the relay's duplicate-endpoint-id report, which iroh 1.0.3 only `warn!`s.
pub mod diag;
/// Peer address resolution that needs no external infrastructure (#68).
pub mod discovery;
#[doc(hidden)]
pub mod ipc;
#[doc(hidden)]
pub mod limits;
pub mod node;
#[doc(hidden)]
pub mod pairing;
pub mod paths;
#[doc(hidden)]
pub mod roster;
#[doc(hidden)]
pub mod stream;
#[doc(hidden)]
pub mod util;
