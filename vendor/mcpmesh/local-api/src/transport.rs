//! The platform local-endpoint seam: ONE narrow API — `connect_local`,
//! `bind_local`, `LocalListener::accept`, `authorize_local_peer`, `split_local` — with
//! per-platform impls. Unix: the family's hardened UDS rule (0700 symlink-refused dir,
//! 0600 socket, same-euid peer gate). Windows: an owner-only-DACL named pipe (the
//! kernel enforces same-user at connect; see `windows.rs`).
//! Everything above this seam is platform-identical: the endpoint value is a `Path`
//! on both (a socket path / a `\\.\pipe\…` name), and both stream types are
//! `AsyncRead + AsyncWrite`.

#[cfg(unix)]
mod unix;
#[cfg(all(unix, feature = "service"))]
pub use unix::{LocalListener, authorize_local_peer, bind_local};
#[cfg(unix)]
pub use unix::{LocalReadHalf, LocalStream, LocalWriteHalf, connect_local, split_local};

#[cfg(windows)]
mod windows;
#[cfg(all(windows, feature = "service"))]
pub use windows::{LocalListener, authorize_local_peer, bind_local};
#[cfg(windows)]
pub use windows::{LocalReadHalf, LocalStream, LocalWriteHalf, connect_local, split_local};

// The unix-native hardened-UDS names, re-exported so `service.rs` can keep the plugin
// seam `mcpmesh_local_api::service::{ensure_private_dir, bind_uds, check_peer_uid}`
// resolving unchanged (the private monorepo depends on these exact paths + signatures).
#[cfg(all(unix, feature = "service"))]
pub use unix::{bind_uds, check_peer_uid, ensure_private_dir};
