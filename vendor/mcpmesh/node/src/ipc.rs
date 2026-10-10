//! Framed local-endpoint transport for mcpmesh-local/1 (UDS on unix; owner-only named pipe
//! on windows). Reuses the family NDJSON codec (`mcpmesh_net::framing`) —
//! "one codec everywhere". Same-uid clients are fully trusted; the peer gate bounds
//! OTHER users only. These wrappers delegate to the platform seam
//! ([`mcpmesh_local_api::transport`]); the peer-uid check below is the UNIX arm of it.
//!
//! Peer-uid API: `tokio::net::UnixStream::peer_cred()` supplies
//! the connecting process's uid cross-platform (Linux `SO_PEERCRED`, macOS `LOCAL_PEERCRED`
//! / `getpeereid`) with tokio owning the platform `unsafe` internally — so our code needs
//! neither the `nix` crate nor inline `unsafe`. Our own uid comes from
//! `rustix::process::geteuid()` (a safe wrapper; rustix is already in the tree via iroh).
//! The invariant: refuse other users.
//! (On windows the seam's arm relies on the owner-only pipe DACL instead — see the seam.)
use anyhow::{Context, Result};
use mcpmesh_local_api::transport::{LocalListener, LocalStream};
use std::path::Path;

/// Per-connection frame cap for the control wire (16 MiB). Shared by the
/// control server and client so both size the `FrameReader` identically.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Create + security-check the runtime dir: `create_dir_all`, refuse a symlink, chmod 0700,
/// verify we own it. Idempotent — safe to call from both the singleton-lock
/// acquisition (which must place a lock file here before the bind) and `bind_control_socket`.
///
/// The implementation is the plugin seam's [`ensure_private_dir`] — ONE hardened
/// rule for every UDS face in the family (the daemon control socket AND every plugin
/// daemon's socket), so the checks can never drift apart again.
///
/// [`ensure_private_dir`]: mcpmesh_local_api::service::ensure_private_dir
#[cfg(unix)]
pub fn ensure_runtime_dir(dir: &Path) -> Result<()> {
    mcpmesh_local_api::service::ensure_private_dir(dir)
        .with_context(|| format!("secure runtime dir {}", dir.display()))
}

/// Bind the control socket through the platform seam ([`bind_local`]): on unix it
/// security-checks the runtime dir (symlink-refused, 0700, owned by us), removes a stale
/// socket, binds, and chmods 0600; on windows it creates the owner-only-DACL named pipe.
/// Returns the platform-neutral [`LocalListener`].
///
/// The unix stale-socket unlink has NO liveness guard of its own — but `run`'s single-daemon
/// flock runs BEFORE `serve_forever` reaches this bind, so no LIVE daemon can hold the socket
/// when we arrive; the unlink can therefore only ever clear a dead daemon's leftover stub.
/// Windows needs no such argument: it has no flock and no unlink — the pipe bind
/// itself IS the singleton (AddrInUse when a peer daemon already owns the pipe).
///
/// Kept `async` even though [`bind_local`] is synchronous: the smallest diff (the production
/// caller — `daemon::serve_forever` — and the ipc test already `.await` this) at zero behavior
/// cost.
///
/// [`bind_local`]: mcpmesh_local_api::transport::bind_local
pub async fn bind_control_socket(path: &Path) -> Result<LocalListener> {
    mcpmesh_local_api::transport::bind_local(path)
        .with_context(|| format!("bind control socket {}", path.display()))
}

/// Same-user gate on an accepted connection. Returns `Ok(())` iff the seam's
/// platform gate authorizes the peer — the peer-euid check on unix (refuse a different uid or
/// an unreadable credential), the owner-only pipe DACL on windows (the kernel already refused
/// other users at connect). Delegates to the seam's [`authorize_local_peer`], which logs the
/// refusal detail.
///
/// [`authorize_local_peer`]: mcpmesh_local_api::transport::authorize_local_peer
pub fn check_peer(stream: &LocalStream) -> Result<()> {
    anyhow::ensure!(
        mcpmesh_local_api::transport::authorize_local_peer(stream),
        "refusing local connection: peer not authorized (same-user gate refused the connection)"
    );
    Ok(())
}

// Unix-only: the round-trip dials via `LocalStream::connect` + `into_split()` (the
// unix seam internals) and asserts the 0700/0600 UDS mode bits. The windows named-pipe
// equivalent (owner-only DACL) is covered by `mcpmesh_local_api::transport::windows`.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use mcpmesh_local_api::{API_NAME, API_VERSION, Hello};
    use mcpmesh_net::framing::{FrameReader, Inbound, write_frame};
    use std::time::Duration;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn hello_frame_roundtrips_over_real_uds_and_peer_uid_passes() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let dir = tempfile::tempdir().unwrap();
            // Nest under a `mcpmesh` subdir so bind_control_socket exercises the real
            // create-dir-0700 + ownership path.
            let path = dir.path().join("mcpmesh").join("mcpmesh.sock");
            let mut listener = bind_control_socket(&path).await.unwrap();

            // The security guarantees every client depends on: the runtime
            // dir is 0700 and the control socket 0600 — no other user can traverse in or
            // connect.
            {
                use std::os::unix::fs::PermissionsExt;
                let dir_mode = std::fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(dir_mode & 0o777, 0o700, "runtime dir must be 0700");
                let sock_mode = std::fs::metadata(&path).unwrap().permissions().mode();
                assert_eq!(sock_mode & 0o777, 0o600, "control socket must be 0600");
            }

            let expected = Hello {
                api: API_NAME.into(),
                api_version: API_VERSION.into(),
                api_minor: 0,
                stack_version: "0.1.0".into(),
            };
            let server_hello = expected.clone();
            let accept = tokio::spawn(async move {
                let stream = listener.accept().await.unwrap();
                // The connecting client is this same test process → same uid → must pass.
                check_peer(&stream).unwrap();
                let (_r, mut w) = stream.into_split();
                let frame = serde_json::to_value(&server_hello).unwrap();
                write_frame(&mut w, &frame).await.unwrap();
            });

            let client = LocalStream::connect(&path).await.unwrap();
            let (r, _w) = client.into_split();
            let mut reader = FrameReader::new(BufReader::new(r), 16 * 1024 * 1024);
            let got: Hello = match reader.next().await.unwrap().unwrap() {
                Inbound::Frame(v) => serde_json::from_value(v).unwrap(),
                other => panic!("expected a hello frame, got {other:?}"),
            };
            assert_eq!(got.api, "mcpmesh-local/1");
            assert_eq!(got.stack_version, "0.1.0");
            assert_eq!(got, expected);
            accept.await.unwrap();
        })
        .await
        .expect("hello round-trip timed out");
    }
}
