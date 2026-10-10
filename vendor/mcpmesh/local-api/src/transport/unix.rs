//! Unix impl of the transport seam: the ONE hardened UDS rule (moved verbatim from
//! service.rs — same rule, same home, now platform-shaped).
use std::io;
use std::path::Path;
use tokio::net::UnixStream;

pub type LocalStream = UnixStream;
pub type LocalReadHalf = tokio::net::unix::OwnedReadHalf;
pub type LocalWriteHalf = tokio::net::unix::OwnedWriteHalf;

/// Owned split (task-spawnable halves). Zero-cost on unix: `UnixStream::into_split`.
pub fn split_local(stream: LocalStream) -> (LocalReadHalf, LocalWriteHalf) {
    stream.into_split()
}

/// Connect the per-user local endpoint. Unix: plain `UnixStream::connect` (a dead
/// socket yields NotFound/ConnectionRefused — the autostart trigger upstream).
pub async fn connect_local(path: &Path) -> io::Result<LocalStream> {
    UnixStream::connect(path).await
}

#[cfg(feature = "service")]
mod server {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;

    // ── ensure_private_dir, bind_uds, check_peer_uid: moved VERBATIM from service.rs,
    //    visibility now `pub` (they are re-exported by service.rs for the plugin seam) ──

    /// Create + security-check a private runtime dir (ONE hardened rule for every
    /// UDS face in the family; the daemon control socket and the plugin seam both bind through
    /// it): `create_dir_all`, refuse a symlink, chmod 0700, verify we own it. Idempotent.
    ///
    /// The checks are load-bearing, not decorative: `create_dir_all` is a no-op when the dir
    /// already exists, so a pre-existing dir planted by another user — or a symlink redirecting
    /// to one we own (or one we don't) — must be refused before we trust it to hold a socket.
    /// `symlink_metadata` does not follow the link, and it runs BEFORE the chmod so a planted
    /// symlink can never make us chmod its target.
    pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        std::fs::create_dir_all(dir)?;
        let is_symlink = std::fs::symlink_metadata(dir)?.file_type().is_symlink();
        if is_symlink {
            return Err(io::Error::other(format!(
                "runtime dir {} is a symlink; refusing",
                dir.display()
            )));
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let meta = std::fs::metadata(dir)?;
        if meta.uid() != rustix::process::geteuid().as_raw() {
            return Err(io::Error::other(format!(
                "runtime dir {} is not owned by us",
                dir.display()
            )));
        }
        Ok(())
    }

    /// Bind a listener at `path`: harden the parent runtime dir ([`ensure_private_dir`] —
    /// create, refuse-symlink, chmod 0700, verify ownership), remove any stale socket, bind,
    /// and chmod the socket 0600.
    ///
    /// The parent dir is forced private because the `XDG_RUNTIME_DIR`
    /// fallback is `std::env::temp_dir()`, whose subdirs are NOT otherwise guaranteed private.
    /// The 0700 dir + 0600 socket are defense in depth only — [`check_peer_uid`] remains the
    /// real gate on every accepted connection.
    pub fn bind_uds(path: &Path) -> io::Result<UnixListener> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            ensure_private_dir(parent)?;
        }
        // A leftover socket file from a crashed daemon blocks bind with EADDRINUSE.
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)
            .map_err(|e| io::Error::new(e.kind(), format!("bind {path:?}: {e}")))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| io::Error::new(e.kind(), format!("chmod 0600 {path:?}: {e}")))?;
        Ok(listener)
    }

    /// Is this connection's peer the same uid as us? `false` (refuse) on a different uid OR an
    /// unreadable peer credential — default-deny, defense in depth beyond the 0600 socket.
    // Implementation: `UnixStream::peer_cred()` -> `UCred::uid()`; `rustix::process::geteuid()`.
    pub fn check_peer_uid(stream: &UnixStream) -> bool {
        let Ok(cred) = stream.peer_cred() else {
            tracing::warn!("peer_cred unreadable: refusing local connection");
            return false;
        };
        let peer = cred.uid();
        let me = rustix::process::geteuid().as_raw();
        if peer != me {
            tracing::warn!(peer, me, "refusing cross-uid local connection");
            return false;
        }
        true
    }

    pub struct LocalListener(UnixListener);

    impl LocalListener {
        /// Accept one connection. `&mut self` for windows-API parity (the pipe
        /// listener must rotate instances); harmless here.
        pub async fn accept(&mut self) -> io::Result<LocalStream> {
            let (stream, _addr) = self.0.accept().await?;
            Ok(stream)
        }
    }

    /// Same-user gate on an accepted connection. Unix: the peer-euid check (the REAL
    /// authorization gate, defense in depth beyond the 0600 socket). On Windows the
    /// owner-only DACL already refused other users at connect, so its impl is `true`.
    pub fn authorize_local_peer(stream: &LocalStream) -> bool {
        check_peer_uid(stream)
    }

    /// Bind the per-user local endpoint at `path`, fully hardened for the platform:
    /// parent dir 0700 + symlink-refused + owned, stale socket removed,
    /// socket 0600. (On Windows this is a first-instance owner-only-DACL pipe and a
    /// second daemon's bind fails AddrInUse — the Windows singleton.)
    pub fn bind_local(path: &Path) -> io::Result<LocalListener> {
        Ok(LocalListener(bind_uds(path)?))
    }
}
#[cfg(feature = "service")]
pub use server::*;
