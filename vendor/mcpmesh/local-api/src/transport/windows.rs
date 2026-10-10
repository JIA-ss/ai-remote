//! Windows impl of the transport seam: a per-user named pipe with an OWNER-ONLY DACL.
//! The DACL is the platform equivalent of the whole unix rule (0700 dir,
//! 0600 socket, peer-euid gate): the kernel refuses a cross-user connect outright,
//! so [`authorize_local_peer`] is trivially true post-accept. `first_pipe_instance`
//! doubles as the per-user single-daemon lock: a second bind fails and is
//! mapped to [`io::ErrorKind::AddrInUse`].
//!
//! Layout mirrors `unix.rs`: the client surface (stream type, halves, `split_local`,
//! `connect_local`) is unconditional under the `client` feature; the listener/bind/
//! authorize half + the `winsec` FFI leaf live in a `#[cfg(feature = "service")]`
//! region below. Unlike `unix.rs` these service items are not wrapped in a `mod
//! server` submodule — the in-file `#[cfg(test)]` block must reach `winsec` and the
//! bind/accept fns as siblings, and a nested private `winsec` would be invisible to a
//! sibling `tests` module. Each service item therefore carries its own gate.
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer};

/// ERROR_PIPE_BUSY: all server instances are momentarily taken (the server rotates a
/// fresh instance right after each accept, so this is a sub-millisecond window).
const PIPE_BUSY: i32 = 231;

/// ERROR_ACCESS_DENIED: what a second `first_pipe_instance` create fails with — the
/// "another daemon owns the pipe" signal.
const ACCESS_DENIED: i32 = 5;

/// A connected local endpoint: either the server side of a pipe instance we accepted,
/// or the client side we dialed. Both are `AsyncRead + AsyncWrite`; everything above
/// the seam treats them identically. `Debug` is idiomatic for a public stream type —
/// parity with unix's `UnixStream`, which is also `Debug`.
#[derive(Debug)]
pub enum LocalStream {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}

/// Forward a pinned poll to whichever inner pipe half the `LocalStream` wraps.
macro_rules! delegate {
    ($self:ident, $method:ident, $($arg:expr),*) => {
        match $self.get_mut() {
            LocalStream::Server(s) => Pin::new(s).$method($($arg),*),
            LocalStream::Client(c) => Pin::new(c).$method($($arg),*),
        }
    };
}

impl AsyncRead for LocalStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        delegate!(self, poll_read, cx, buf)
    }
}

impl AsyncWrite for LocalStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        delegate!(self, poll_write, cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, poll_flush, cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        delegate!(self, poll_shutdown, cx)
    }
}

pub type LocalReadHalf = tokio::io::ReadHalf<LocalStream>;
pub type LocalWriteHalf = tokio::io::WriteHalf<LocalStream>;

/// Owned split. Windows: `tokio::io::split` (a `BiLock`) — the two pipe types have no
/// native owned split, and a serial control plane never contends the lock.
pub fn split_local(stream: LocalStream) -> (LocalReadHalf, LocalWriteHalf) {
    tokio::io::split(stream)
}

/// Connect the per-user local endpoint with a bounded ERROR_PIPE_BUSY retry:
/// the server pre-creates the next instance right after each accept, so "busy"
/// is a sub-millisecond window; 50×20ms is a generous bound. A missing pipe surfaces
/// as `NotFound` — the autostart trigger, exactly as a dead unix socket does.
pub async fn connect_local(path: &Path) -> io::Result<LocalStream> {
    let mut tries = 0u32;
    loop {
        match ClientOptions::new().open(path) {
            Ok(client) => return Ok(LocalStream::Client(client)),
            Err(e) if e.raw_os_error() == Some(PIPE_BUSY) && tries < 50 => {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

// ── service half: listener + bind + the owner-only-DACL FFI ─────────────────────────

/// Post-accept same-user gate. Trivially true here: the owner-only DACL already made
/// the kernel refuse any cross-user connect, so there is no peer
/// credential left to check — the DACL *is* the gate, applied before accept.
#[cfg(feature = "service")]
pub fn authorize_local_peer(_stream: &LocalStream) -> bool {
    true
}

/// The accepting side of the per-user pipe. Holds the path (to mint replacement
/// instances) and the one pre-created instance the next client will connect to.
#[cfg(feature = "service")]
#[derive(Debug)]
pub struct LocalListener {
    path: std::path::PathBuf,
    /// The pre-created instance the next client will connect to. Rotated in
    /// [`LocalListener::accept`]: the replacement is created BEFORE the connected one
    /// is handed out, so there is never a moment with no listening instance (a client
    /// dialing between accepts hits a live instance, not a NotFound race).
    next: NamedPipeServer,
}

#[cfg(feature = "service")]
impl LocalListener {
    /// Accept one connection: await a client on the pending instance, then swap in a
    /// freshly created (non-first) instance before returning the connected one.
    pub async fn accept(&mut self) -> io::Result<LocalStream> {
        self.next.connect().await?;
        let connected = std::mem::replace(&mut self.next, create_instance(&self.path, false)?);
        Ok(LocalStream::Server(connected))
    }
}

/// Bind the per-user local endpoint at `path`: the FIRST pipe instance carrying the
/// owner-only DACL. `first_pipe_instance(true)` doubles as the single-daemon lock —
/// a second daemon's first-instance create fails ERROR_ACCESS_DENIED, which we map to
/// [`io::ErrorKind::AddrInUse`] so callers see the same "already running" shape the
/// unix flock produces.
#[cfg(feature = "service")]
pub fn bind_local(path: &Path) -> io::Result<LocalListener> {
    let first = create_instance(path, true).map_err(|e| {
        if e.raw_os_error() == Some(ACCESS_DENIED) {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "pipe {} already owned by another daemon: {e}",
                    path.display()
                ),
            )
        } else {
            io::Error::new(e.kind(), format!("bind {}: {e}", path.display()))
        }
    })?;
    Ok(LocalListener {
        path: path.to_path_buf(),
        next: first,
    })
}

/// One pipe server instance carrying the owner-only security descriptor. `first` is
/// true only for the very first instance (the singleton-lock semantics live there).
#[cfg(feature = "service")]
fn create_instance(path: &Path, first: bool) -> io::Result<NamedPipeServer> {
    use tokio::net::windows::named_pipe::ServerOptions;
    // The token→SID→descriptor dance is recomputed on every accepted connection —
    // deliberate; control-plane accept frequency makes caching not worth the lifetime
    // plumbing.
    let sd = winsec::owner_only_attributes()?;
    let mut opts = ServerOptions::new();
    opts.first_pipe_instance(first);
    // The raw-pointer contract is "a valid SECURITY_ATTRIBUTES for the duration of the
    // call"; `sd` owns its descriptor and outlives the `create` call below.
    sd.create(&mut opts, path)
}

/// The one unsafe leaf: build a `SECURITY_ATTRIBUTES` whose DACL grants
/// GENERIC_ALL to the CURRENT USER'S SID and to no one else — the peer-credential
/// equivalent. SDDL keeps it auditable: `D:P(A;;GA;;;<sid>)` = protected DACL (no
/// inherited ACEs), exactly one allow-ACE, no inheritance out. `sddl_for_current_user`
/// is split out so a windows unit test can assert the string shape without a pipe.
#[cfg(feature = "service")]
#[allow(unsafe_code)]
mod winsec {
    use super::*;
    use core::ffi::c_void;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::core::PWSTR;

    /// An owned security descriptor (LocalAlloc'd by the SDDL conversion). Freed on
    /// drop; not `Copy`/`Clone`, so it is freed exactly once.
    pub(super) struct OwnedAttributes {
        descriptor: *mut c_void,
    }

    impl Drop for OwnedAttributes {
        fn drop(&mut self) {
            // SAFETY: `descriptor` came from
            // ConvertStringSecurityDescriptorToSecurityDescriptorW, which documents
            // LocalFree as its release fn; freed exactly once (no Copy/Clone).
            unsafe { LocalFree(self.descriptor as HLOCAL) };
        }
    }

    impl OwnedAttributes {
        /// Create a pipe instance with this descriptor's owner-only DACL.
        pub(super) fn create(
            &self,
            opts: &mut tokio::net::windows::named_pipe::ServerOptions,
            path: &Path,
        ) -> io::Result<NamedPipeServer> {
            let mut sa = SECURITY_ATTRIBUTES {
                nLength: core::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.descriptor,
                bInheritHandle: 0,
            };
            // SAFETY: `sa` and the descriptor it points at are valid for the whole
            // call; tokio copies what it needs into the pipe before returning.
            unsafe {
                opts.create_with_security_attributes_raw(path, (&raw mut sa).cast::<c_void>())
            }
        }
    }

    /// The current process user's SID as an SDDL string (e.g. `S-1-5-21-…`). Follows
    /// the standard token dance: open our process token, size-probe then read the
    /// `TokenUser` info, stringify the SID, copy it out, and release every OS handle
    /// and buffer we allocated.
    fn current_user_sid() -> io::Result<String> {
        let mut token: HANDLE = core::ptr::null_mut();
        // SAFETY: GetCurrentProcess returns a pseudo-handle (never closed); `token` is
        // a valid out-param filled with a real, closeable handle on success.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }

        // Size probe: null buffer / zero length → the call "fails" (ERROR_INSUFFICIENT
        // _BUFFER) but writes the required byte count into `len`. We only read `len`.
        let mut len: u32 = 0;
        // SAFETY: `token` is valid; a null info buffer with length 0 is the documented
        // way to request the required size in `len`.
        unsafe { GetTokenInformation(token, TokenUser, core::ptr::null_mut(), 0, &mut len) };

        let mut buf = vec![0u8; len as usize];
        // SAFETY: `token` is valid; `buf` is `len` bytes; on success it holds a
        // well-formed TOKEN_USER.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr().cast::<c_void>(),
                len,
                &mut len,
            )
        };
        if ok == 0 {
            let e = io::Error::last_os_error();
            // SAFETY: `token` is a valid handle from OpenProcessToken, closed once.
            unsafe { CloseHandle(token) };
            return Err(e);
        }

        // SAFETY: the sized GetTokenInformation call above filled `buf` with a TOKEN_USER
        // followed by the SID it points into; read_unaligned copies the struct without
        // requiring the Vec<u8> allocation to be 8-aligned (Vec<u8> guarantees only 1).
        // The interior `Sid` pointer stays valid because `buf` outlives the convert call below.
        let tu: TOKEN_USER = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast::<TOKEN_USER>()) };
        let sid: PSID = tu.User.Sid;

        let mut pwstr: PWSTR = core::ptr::null_mut();
        // SAFETY: `sid` is valid for the call; `pwstr` receives a LocalAlloc'd
        // NUL-terminated UTF-16 string on success.
        let ok = unsafe { ConvertSidToStringSidW(sid, &mut pwstr) };
        // `buf` (and thus `sid`) is no longer needed once the string is materialized;
        // the token likewise. Close/read below.
        if ok == 0 {
            let e = io::Error::last_os_error();
            // SAFETY: `token` is a valid handle from OpenProcessToken, closed once.
            unsafe { CloseHandle(token) };
            return Err(e);
        }

        // SAFETY: `pwstr` points to a NUL-terminated UTF-16 buffer we own until
        // LocalFree; walk to the NUL, then view the run as a slice.
        let s = unsafe {
            let mut n = 0usize;
            while *pwstr.add(n) != 0 {
                n += 1;
            }
            String::from_utf16_lossy(core::slice::from_raw_parts(pwstr, n))
        };

        // SAFETY: `pwstr` came from ConvertSidToStringSidW; LocalFree is its documented
        // release fn, called exactly once.
        unsafe { LocalFree(pwstr as HLOCAL) };
        // SAFETY: `token` is a valid handle from OpenProcessToken, closed exactly once.
        unsafe { CloseHandle(token) };
        Ok(s)
    }

    pub(super) fn sddl_for_current_user() -> io::Result<String> {
        Ok(format!("D:P(A;;GA;;;{})", current_user_sid()?))
    }

    pub(super) fn owner_only_attributes() -> io::Result<OwnedAttributes> {
        let sddl: Vec<u16> = sddl_for_current_user()?.encode_utf16().chain([0]).collect();
        let mut descriptor: *mut c_void = core::ptr::null_mut();
        // SAFETY: `sddl` is a NUL-terminated UTF-16 string valid for the call;
        // `descriptor` receives a LocalAlloc'd buffer owned by OwnedAttributes (freed
        // in its Drop). The size out-param is optional and passed null.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                core::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedAttributes { descriptor })
    }
}

// The Windows security tests below RUN only on the windows CI leg; on the macOS host
// they are cross-compile-checked via `--target x86_64-pc-windows-msvc --profile test`.
// NOTE: cross-user DENIAL cannot be exercised on a single-user CI runner — the
// `sddl_is_owner_only` DACL-string assertion plus the design doc carry that guarantee;
// do not mistake this suite for cross-user coverage.
#[cfg(all(test, feature = "service"))]
mod tests {
    use super::*;

    /// The descriptor is exactly one protected allow-ACE for OUR sid.
    #[test]
    fn sddl_is_owner_only() {
        let sddl = winsec::sddl_for_current_user().unwrap();
        assert!(
            sddl.starts_with("D:P(A;;GA;;;S-1-"),
            "one protected allow-ACE: {sddl}"
        );
        assert_eq!(sddl.matches("(A;").count(), 1, "no second ACE: {sddl}");
    }

    /// Bind + same-user connect + a frame both ways; a second bind is AddrInUse (the
    /// singleton), and the owner-only DACL makes `authorize_local_peer` trivially true.
    #[tokio::test]
    async fn pipe_binds_accepts_same_user_and_is_singleton() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let path = Path::new(r"\\.\pipe\mcpmesh-transport-test");
        let mut listener = bind_local(path).unwrap();
        let second = bind_local(path);
        assert_eq!(second.unwrap_err().kind(), io::ErrorKind::AddrInUse);

        let (client, server) = tokio::join!(connect_local(path), listener.accept());
        let (mut client, mut server) = (client.unwrap(), server.unwrap());
        assert!(authorize_local_peer(&server));
        client.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
        server.write_all(b"pong").await.unwrap();
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
    }
}
