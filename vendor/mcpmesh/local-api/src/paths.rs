//! Shared paths rule: resolved per-platform in ONE place (XDG on unix;
//! APPDATA/LOCALAPPDATA + named-pipe names on windows). Featureless and std-only —
//! it lives on the vocabulary crate precisely so EVERY consumer (the daemon, the CLI,
//! and plugins, which are barred from `mcpmesh-trust`) resolves the same endpoint from
//! the same rule instead of a per-crate replica. `mcpmesh_trust::paths` re-exports it.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The longest a Unix control-socket path may be (`sockaddr_un.sun_path`, minus the NUL): 103 on
/// macOS/BSD, 107 on Linux. Used to keep a `--profile`/`MCPMESH_HOME` socket bindable no matter
/// how deep the profile root sits (#32); the CLI keeps its own bind-time guard as a backstop.
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd"
))]
pub const MAX_SOCKET_PATH: usize = 103;
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd"
)))]
pub const MAX_SOCKET_PATH: usize = 107;

/// In-process profile-root override, set by `mcpmesh --profile <dir>` / `--home <dir>` before
/// dispatch. Preferred over mutating env because `std::env::set_var` is `unsafe` under the
/// workspace's `forbid(unsafe_code)` (and edition 2024). First set wins.
static ROOT_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Pin the profile root for THIS process (the in-process form of `--profile`/`--home`). Returns
/// `Err(dir)` if a root was already pinned (first set wins). The spawned daemon inherits the same
/// root via the `MCPMESH_HOME` env var the launcher sets, so both ends resolve identically.
pub fn set_root(dir: PathBuf) -> Result<(), PathBuf> {
    ROOT_OVERRIDE.set(dir)
}

/// The active profile root: the in-process override if set, else an absolute non-empty
/// `MCPMESH_HOME`. `None` = the standard XDG (unix) / APPDATA (windows) layout. When `Some`, ALL
/// state — config, data, state, and the runtime socket — roots under this ONE directory (#32),
/// so isolating an instance takes one knob instead of overriding five XDG vars.
pub fn profile_root() -> Option<PathBuf> {
    if let Some(p) = ROOT_OVERRIDE.get() {
        return Some(p.clone());
    }
    std::env::var("MCPMESH_HOME")
        .ok()
        .filter(|s| !s.is_empty() && Path::new(s).is_absolute())
        .map(PathBuf::from)
}

/// The runtime dir under a profile root — `<root>/run` when the resulting `mcpmesh.sock` path fits
/// within [`MAX_SOCKET_PATH`], else a short deterministic `<tmp>/mcpmesh-<fnv8(root)>` so a DEEP
/// profile root still yields a bindable socket. Deterministic in `root`, so the daemon and any
/// client resolve the same path. Pure (no env) for unit-testing.
fn runtime_under_root(root: &Path, tmp: &Path) -> PathBuf {
    let natural = root.join("run");
    let sock_len = natural.join("mcpmesh.sock").as_os_str().len();
    if sock_len <= MAX_SOCKET_PATH {
        natural
    } else {
        tmp.join(format!("mcpmesh-{}", fnv8(&root.to_string_lossy())))
    }
}

/// The ONE XDG-basedir rule shared by [`config_dir`]/[`data_dir`]/[`state_dir`]:
/// `$<var>/mcpmesh` when the var is set, non-empty, and absolute; otherwise
/// `$HOME/<segments…>/mcpmesh`. A missing/empty `HOME` is a typed error — the same
/// posture as [`runtime_dir`] (never a panic). Unix-only wiring (see [`win_dir`] for
/// the Windows sibling); `#[cfg(unix)]` because it is otherwise dead on Windows.
#[cfg(unix)]
fn xdg_dir(var: &str, home_segments: &[&str]) -> std::io::Result<PathBuf> {
    let xdg = std::env::var(var).ok();
    let home = std::env::var("HOME").ok();
    xdg_dir_from(var, xdg.as_deref(), home.as_deref(), home_segments)
}

/// Pure core of the XDG rule (the same env-free split as [`runtime_dir_from`], so it is
/// unit-testable without mutating process env). `xdg` is the raw `$<var>` value if set;
/// `home` is the raw `$HOME` value if set. Deliberately un-cfg'd (like [`win_dir_from`]) so its
/// unit tests run on every host; its only non-test caller is the `#[cfg(unix)]` [`xdg_dir`], so
/// plain windows `lib` builds see it as unused — `allow(dead_code)` rather than `#[cfg(unix)]` here.
#[allow(dead_code)]
fn xdg_dir_from(
    var: &str,
    xdg: Option<&str>,
    home: Option<&str>,
    home_segments: &[&str],
) -> std::io::Result<PathBuf> {
    if let Some(x) = xdg
        && !x.is_empty()
        && std::path::Path::new(x).is_absolute()
    {
        return Ok(PathBuf::from(x).join("mcpmesh"));
    }
    match home {
        Some(h) if !h.is_empty() => {
            let mut dir = PathBuf::from(h);
            for seg in home_segments {
                dir.push(seg);
            }
            dir.push("mcpmesh");
            Ok(dir)
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("HOME is not set; set HOME or {var} to an absolute path"),
        )),
    }
}

/// Windows sibling of [`xdg_dir_from`]: an absolute XDG override still wins (so the
/// family's env-var test isolation works on every platform); otherwise
/// `<base_env>\mcpmesh\<segments…>` where `base_env` is `%APPDATA%` (config) or
/// `%LOCALAPPDATA%` (data/state). Missing base env is a typed error (HOME-parity).
/// Deliberately un-cfg'd (like [`xdg_dir_from`]) so its unit tests run on every host;
/// its only non-test caller is the `#[cfg(windows)]` [`win_dir`], so plain unix `lib`
/// builds see it as unused — `allow(dead_code)` rather than `#[cfg(windows)]` here.
#[allow(dead_code)]
fn win_dir_from(
    xdg: Option<&str>,
    base: Option<&str>,
    segments: &[&str],
) -> std::io::Result<PathBuf> {
    if let Some(x) = xdg
        && !x.is_empty()
        && std::path::Path::new(x).is_absolute()
    {
        return Ok(PathBuf::from(x).join("mcpmesh"));
    }
    match base {
        Some(b) if !b.is_empty() => {
            let mut dir = PathBuf::from(b);
            dir.push("mcpmesh");
            for seg in segments {
                dir.push(seg);
            }
            Ok(dir)
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "APPDATA/LOCALAPPDATA is not set; set it or the XDG override to an absolute path",
        )),
    }
}

#[cfg(windows)]
fn win_dir(xdg_var: &str, base_var: &str, segments: &[&str]) -> std::io::Result<PathBuf> {
    let xdg = std::env::var(xdg_var).ok();
    let base = std::env::var(base_var).ok();
    win_dir_from(xdg.as_deref(), base.as_deref(), segments)
}

/// FNV-1a, folded to 32 bits — deterministic across processes AND rustc versions
/// (a `DefaultHasher` is not), because the daemon and a plugin daemon may be
/// different binaries that must resolve the SAME pipe name. Un-cfg'd like
/// [`win_dir_from`] so its behavior is tested on every host; only used outside tests
/// on `#[cfg(windows)]`, hence the `allow`.
#[allow(dead_code)]
fn fnv8(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{:08x}", ((h >> 32) ^ h) as u32)
}

/// keep `[a-z0-9_-]`, lowercase the rest in, map anything else to `-`. Un-cfg'd like
/// [`win_dir_from`]; see its comment for why the unused-on-unix `allow` is here
/// instead of a `#[cfg(windows)]` gate.
#[allow(dead_code)]
fn sanitize_pipe_component(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '_' | '-' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '-',
        })
        .collect()
}

/// Windows local endpoint: a named pipe, not a filesystem socket.
/// `\\.\pipe\mcpmesh-<domain>-<user>[-<fnv8(XDG_RUNTIME_DIR)>]`. The name is only a
/// rendezvous label — same-user enforcement is the pipe's owner-only DACL
/// (`mcpmesh-local-api`'s bind), never the name. USERDOMAIN disambiguates a local and
/// a domain account sharing a username; XDG_RUNTIME_DIR (set by hermetic tests on
/// every platform) isolates parallel test daemons exactly as it does on Unix.
/// Un-cfg'd like [`win_dir_from`]; see its comment for why the unused-on-unix
/// `allow` is here instead of a `#[cfg(windows)]` gate.
#[allow(dead_code)]
fn windows_pipe_name_from(
    domain: Option<&str>,
    user: Option<&str>,
    xdg: Option<&str>,
) -> std::io::Result<PathBuf> {
    let user = user.filter(|u| !u.is_empty()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "USERNAME is not set; cannot derive a per-user pipe name",
        )
    })?;
    let mut name = format!(
        r"\\.\pipe\mcpmesh-{}-{}",
        sanitize_pipe_component(domain.unwrap_or("local")),
        sanitize_pipe_component(user)
    );
    if let Some(x) = xdg
        && !x.is_empty()
    {
        name.push('-');
        name.push_str(&fnv8(x));
    }
    Ok(PathBuf::from(name))
}

#[cfg(windows)]
fn windows_pipe_name() -> std::io::Result<PathBuf> {
    let domain = std::env::var("USERDOMAIN").ok();
    let user = std::env::var("USERNAME").ok();
    // A profile root (#32) isolates the pipe just as XDG_RUNTIME_DIR does — its path hashes into
    // the name suffix, so two profiles on one machine get distinct pipes and rendezvous by
    // construction. Falls back to XDG_RUNTIME_DIR when no profile is active.
    let iso = profile_root()
        .map(|r| r.to_string_lossy().into_owned())
        .or_else(|| std::env::var("XDG_RUNTIME_DIR").ok());
    windows_pipe_name_from(domain.as_deref(), user.as_deref(), iso.as_deref())
}

/// Per-platform config dir: `$XDG_CONFIG_HOME/mcpmesh` when that var is set,
/// non-empty, and absolute; otherwise `$HOME/.config/mcpmesh` (unix). On Windows,
/// `%APPDATA%\mcpmesh` (an absolute `XDG_CONFIG_HOME` override still wins, for test
/// isolation).
pub fn config_dir() -> std::io::Result<PathBuf> {
    if let Some(root) = profile_root() {
        return Ok(root.join("config"));
    }
    #[cfg(unix)]
    return xdg_dir("XDG_CONFIG_HOME", &[".config"]);
    #[cfg(windows)]
    return win_dir("XDG_CONFIG_HOME", "APPDATA", &[]);
}

pub fn default_device_key_path() -> std::io::Result<PathBuf> {
    Ok(config_dir()?.join("device.key"))
}

pub fn default_config_path() -> std::io::Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

/// Per-platform runtime dir for the control socket: `$XDG_RUNTIME_DIR/mcpmesh`
/// when that var is set, non-empty, and absolute (Linux); otherwise `$TMPDIR/mcpmesh`, or
/// `std::env::temp_dir()/mcpmesh` when `TMPDIR` is unset (macOS — its per-user `$TMPDIR` is
/// itself private). Returns the path only; the daemon creates it 0700 + verifies
/// ownership before binding (a bind-time concern — see cli `ipc::bind_control_socket`).
pub fn runtime_dir() -> std::io::Result<PathBuf> {
    let tmp = std::env::var("TMPDIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    if let Some(root) = profile_root() {
        // A profile roots the socket under its own tree, or a short hashed tmp dir when the tree
        // is too deep for SUN_LEN — either way deterministic in `root`, so all ends rendezvous.
        return Ok(runtime_under_root(&root, &tmp));
    }
    let xdg = std::env::var("XDG_RUNTIME_DIR").ok();
    runtime_dir_from(xdg.as_deref(), tmp)
}

/// Pure core of the runtime-dir rule, split out so the env logic is unit-testable without
/// mutating process env (no `temp_env` dev-dep). `xdg` is the raw `$XDG_RUNTIME_DIR`
/// value if the var is set; `tmp` is the already-resolved fallback base. Prefers XDG
/// iff it is non-empty and absolute; always returns the `mcpmesh` subdir.
///
/// Guards absoluteness: a relative base (e.g. `TMPDIR=""` with no XDG, where
/// `std::env::temp_dir()` also yields `""`) would place the control socket in the
/// process CWD — a per-user-endpoint violation and a double-daemon rendezvous hazard.
/// Such a base is an error, not a silent relative path.
fn runtime_dir_from(xdg: Option<&str>, tmp: PathBuf) -> std::io::Result<PathBuf> {
    if let Some(x) = xdg
        && !x.is_empty()
        && std::path::Path::new(x).is_absolute()
    {
        return Ok(PathBuf::from(x).join("mcpmesh"));
    }
    let dir = tmp.join("mcpmesh");
    if !dir.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "could not resolve a per-user runtime dir; \
             set XDG_RUNTIME_DIR or TMPDIR to an absolute path",
        ));
    }
    Ok(dir)
}

/// The local control endpoint, resolved for THIS platform: a Unix socket at
/// `<runtime_dir>/mcpmesh.sock` on unix. On Windows there is no per-user runtime dir with
/// the right ACL semantics, so the control plane is a named pipe instead — see the private
/// `windows_pipe_name` helper; the returned `PathBuf` is the pipe name (`\\.\pipe\…`), which is
/// what `transport::connect_local`/`bind_local` dial. The ONE resolver both wire ends
/// share: the daemon binds exactly what this returns, so clients rendezvous by
/// construction, never by copied formula.
pub fn default_endpoint() -> std::io::Result<PathBuf> {
    #[cfg(unix)]
    return Ok(runtime_dir()?.join("mcpmesh.sock"));
    #[cfg(windows)]
    return windows_pipe_name();
}

/// Per-platform data dir for durable state: `$XDG_DATA_HOME/mcpmesh` when that
/// var is set, non-empty, and absolute; otherwise `$HOME/.local/share/mcpmesh` (unix).
/// `state.redb` (the peer allowlist) lives here. Unlike the runtime dir (ephemeral, per-boot),
/// this is durable across reboots. On Windows, `%LOCALAPPDATA%\mcpmesh\data` (LOCALAPPDATA,
/// not APPDATA — this data need not roam with the user profile).
pub fn data_dir() -> std::io::Result<PathBuf> {
    if let Some(root) = profile_root() {
        return Ok(root.join("data"));
    }
    #[cfg(unix)]
    return xdg_dir("XDG_DATA_HOME", &[".local", "share"]);
    #[cfg(windows)]
    return win_dir("XDG_DATA_HOME", "LOCALAPPDATA", &["data"]);
}

/// The peer allowlist store path (`<data_dir>/state.redb`).
pub fn default_state_db_path() -> std::io::Result<PathBuf> {
    Ok(data_dir()?.join("state.redb"))
}

/// Outstanding pairing invites (`<data_dir>/invites.json`, #87b).
///
/// Holds BEARER SECRETS and is written `0600`. Beside the trust store because it is durable
/// per-node state; deleting it is a safe operator action that invalidates every outstanding invite.
pub fn default_invites_path() -> std::io::Result<PathBuf> {
    Ok(data_dir()?.join("invites.json"))
}

/// The gated app-blob store directory (`<data_dir>/blobs/` — the daemon's gated iroh-blobs store).
pub fn default_blobs_dir() -> std::io::Result<PathBuf> {
    Ok(data_dir()?.join("blobs"))
}

/// The persisted blob-scope keyed store (`<data_dir>/blob-scopes.redb`). Was one JSON document:
/// `scope_name -> { hashes, grants }`, atomic-write single-writer.
pub fn default_blob_scopes_path() -> std::io::Result<PathBuf> {
    Ok(data_dir()?.join("blob-scopes.redb"))
}

/// Per-platform STATE dir for durable, per-node runtime state: `$XDG_STATE_HOME/mcpmesh`
/// when that var is set, non-empty, and absolute; otherwise `$HOME/.local/state/mcpmesh`. Distinct
/// from `data_dir()` (`~/.local/share`, XDG_DATA_HOME): the XDG basedir spec places *state* data
/// (logs, history — the audit JSONL here) under `~/.local/state`, separate from portable app data.
/// Mirrors the `data_dir()` derivation exactly, swapping the var and the `.local/state` segment. On
/// Windows, `%LOCALAPPDATA%\mcpmesh\state` (mirrors `data_dir()`'s Windows branch, `state` segment).
pub fn state_dir() -> std::io::Result<PathBuf> {
    if let Some(root) = profile_root() {
        return Ok(root.join("state"));
    }
    #[cfg(unix)]
    return xdg_dir("XDG_STATE_HOME", &[".local", "state"]);
    #[cfg(windows)]
    return win_dir("XDG_STATE_HOME", "LOCALAPPDATA", &["state"]);
}

/// The append-only audit-log directory (`<state_dir>/audit/`). One monthly JSONL
/// file per calendar month (`YYYY-MM.jsonl`) lives here; the writer creates the directory lazily on
/// first append. Local-only — nothing here is ever transmitted.
pub fn default_audit_dir() -> std::io::Result<PathBuf> {
    Ok(state_dir()?.join("audit"))
}

/// The installed roster document (`<config_dir>/roster.json`).
pub fn default_roster_path() -> std::io::Result<PathBuf> {
    Ok(config_dir()?.join("roster.json"))
}

/// The per-node roster-freshness sidecar (`<config_dir>/roster.confirmed`). One epoch-seconds
/// integer: the last instant this node validated the installed roster as current via an authenticated
/// channel (a TLS URL poll ≥ installed, a gossip-delivered roster passing validation, or a manual
/// install). Per-node LIVENESS state, NOT a roster-document field — keeps roster.json a pure
/// re-serialization.
pub fn default_roster_confirmed_path() -> std::io::Result<PathBuf> {
    Ok(config_dir()?.join("roster.confirmed"))
}

/// The org-root key (`<config_dir>/org-root.key`) — held by the operator. Present ONLY on
/// the operator's node (minted by `org create`); signs rosters.
pub fn default_org_root_key_path() -> std::io::Result<PathBuf> {
    Ok(config_dir()?.join("org-root.key"))
}

/// This person's user key (`<config_dir>/user.key`). Minted by `join`; binds a
/// person's devices. Present on every enrolled person's device (never moves between machines).
pub fn default_user_key_path() -> std::io::Result<PathBuf> {
    Ok(config_dir()?.join("user.key"))
}

#[cfg(test)]
mod path_tests {
    use super::*;

    // Env-scoping approach (declared delta from the plan's `temp_env` suggestion):
    // rather than mutate process env in tests (which would need the `temp_env` dev-dep
    // plus test serialization), the §13 rule lives in the pure `runtime_dir_from`; we
    // unit-test it directly with explicit inputs. `default_endpoint()` is still
    // exercised against the real env, asserting the shape that holds for any env.

    // Feeds POSIX path literals (`/tmp`), which `Path::is_absolute` + `join` only treat as
    // intended on unix — and the SUN_LEN socket-path concern is unix-only anyway (Windows uses a
    // named pipe, whose isolation rides `windows_pipe_name`). Same gate as the runtime_dir tests.
    #[cfg(unix)]
    #[test]
    fn profile_runtime_uses_the_short_socket_path_for_a_deep_root() {
        // #32: a shallow profile root keeps the socket beside its state (<root>/run); a DEEP root
        // whose <root>/run/mcpmesh.sock would blow SUN_LEN falls back to a short hashed tmp dir —
        // deterministic in the root, so the daemon and its clients still rendezvous.
        let shallow = Path::new("/tmp/p");
        assert_eq!(
            runtime_under_root(shallow, Path::new("/tmp")),
            PathBuf::from("/tmp/p/run")
        );
        let deep = PathBuf::from(format!("/home/user/{}", "very-deep-segment/".repeat(12)));
        let got = runtime_under_root(&deep, Path::new("/tmp"));
        assert!(
            got.join("mcpmesh.sock").as_os_str().len() <= MAX_SOCKET_PATH,
            "deep-root socket must fit SUN_LEN, got {}",
            got.display()
        );
        assert!(
            got.to_string_lossy().starts_with("/tmp/mcpmesh-"),
            "deep root falls back to a short hashed tmp dir, got {}",
            got.display()
        );
        // Deterministic: same root → same short dir.
        assert_eq!(got, runtime_under_root(&deep, Path::new("/tmp")));
    }

    // The next three tests feed the pure cores POSIX literals like `/run/user/1000`,
    // which `Path::is_absolute` only treats as absolute on unix — and the xdg/runtime
    // rules they exercise are wired only on the unix arm anyway, so they run there.
    #[cfg(unix)]
    #[test]
    fn runtime_dir_prefers_xdg_when_set() {
        assert_eq!(
            runtime_dir_from(Some("/run/user/1000"), PathBuf::from("/tmp")).unwrap(),
            PathBuf::from("/run/user/1000/mcpmesh")
        );
    }

    #[cfg(unix)]
    #[test]
    fn runtime_dir_falls_back_to_tmp_when_xdg_absent_empty_or_relative() {
        let tmp = PathBuf::from("/var/folders/xx");
        let want = PathBuf::from("/var/folders/xx/mcpmesh");
        assert_eq!(runtime_dir_from(None, tmp.clone()).unwrap(), want);
        assert_eq!(runtime_dir_from(Some(""), tmp.clone()).unwrap(), want);
        // A relative XDG value is rejected (§13: must be absolute) → tmp fallback.
        assert_eq!(runtime_dir_from(Some("relative/dir"), tmp).unwrap(), want);
    }

    #[test]
    fn empty_tmpdir_without_xdg_errors_not_relative() {
        // TMPDIR="" with no XDG resolves the base to "" → a relative "mcpmesh" dir, which
        // would drop the control socket in the process CWD. The guard must Err (§13),
        // never return a relative path.
        let err = runtime_dir_from(None, PathBuf::from("")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    // Unix-only: the control endpoint is a filesystem socket path under the runtime dir.
    #[cfg(unix)]
    #[test]
    fn default_endpoint_is_under_runtime_dir() {
        // Holds for any ambient env: the socket is <runtime_dir>/mcpmesh.sock, runtime_dir
        // always ends in `mcpmesh`, and the resolved path is absolute (§13).
        let sock = default_endpoint().unwrap();
        assert!(sock.ends_with("mcpmesh/mcpmesh.sock"));
        assert!(sock.is_absolute());
    }

    // Windows twin: the control endpoint is a per-user named pipe, not a filesystem socket.
    #[cfg(windows)]
    #[test]
    fn default_endpoint_is_a_per_user_pipe_name() {
        let sock = default_endpoint().unwrap();
        assert!(sock.to_string_lossy().starts_with(r"\\.\pipe\mcpmesh-"));
    }

    #[test]
    fn audit_dir_is_state_dir_slash_audit() {
        // Holds for any ambient env ON EITHER PLATFORM: the audit dir is <state_dir>/audit
        // and absolute (spec §13; unix `~/.local/state/mcpmesh/audit`, windows
        // `%LOCALAPPDATA%\mcpmesh\state\audit` — different tails, same parent relation).
        let audit = default_audit_dir().unwrap();
        assert_eq!(audit.file_name().unwrap(), "audit");
        assert!(audit.is_absolute());
        // The audit dir is a child of the state dir (durable, distinct from data_dir()).
        assert_eq!(audit.parent().unwrap(), state_dir().unwrap());
    }

    #[test]
    fn state_dir_prefers_xdg_state_home_when_absolute() {
        // Mirror of the data_dir rule for XDG_STATE_HOME (spec §13). Exercised against the
        // real env: absolute, with a `mcpmesh` path component on either platform (unix ends
        // in `mcpmesh`; windows default is `%LOCALAPPDATA%\mcpmesh\state`).
        let sd = state_dir().unwrap();
        assert!(sd.components().any(|c| c.as_os_str() == "mcpmesh"));
        assert!(sd.is_absolute());
    }

    #[cfg(unix)]
    #[test]
    fn xdg_dir_prefers_the_var_when_absolute_else_home_segments() {
        // The var wins when set + non-empty + absolute (§13).
        assert_eq!(
            xdg_dir_from(
                "XDG_DATA_HOME",
                Some("/xdg/data"),
                Some("/home/u"),
                &[".local", "share"]
            )
            .unwrap(),
            PathBuf::from("/xdg/data/mcpmesh")
        );
        // Absent / empty / relative var → $HOME/<segments>/mcpmesh.
        for bad in [None, Some(""), Some("relative/dir")] {
            assert_eq!(
                xdg_dir_from("XDG_DATA_HOME", bad, Some("/home/u"), &[".local", "share"]).unwrap(),
                PathBuf::from("/home/u/.local/share/mcpmesh")
            );
        }
    }

    #[test]
    fn xdg_dir_without_home_errors_never_panics() {
        // M5: the old code `expect("HOME not set")`-panicked here; the rule now matches
        // runtime_dir's typed-error posture. Empty HOME is as unusable as unset HOME.
        for home in [None, Some("")] {
            let err = xdg_dir_from("XDG_CONFIG_HOME", None, home, &[".config"]).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        }
    }

    #[test]
    fn win_dir_prefers_xdg_override_else_base_env() {
        // XDG override wins when absolute (test isolation on Windows too). The literal is
        // platform-selected because `is_absolute` is platform-semantic: `/xdg/cfg` is not
        // absolute on windows, `C:\xdg\cfg` is not absolute on unix.
        let abs_override = if cfg!(windows) {
            r"C:\xdg\cfg"
        } else {
            "/xdg/cfg"
        };
        assert_eq!(
            win_dir_from(Some(abs_override), Some(r"C:\Users\u\AppData\Roaming"), &[]).unwrap(),
            PathBuf::from(abs_override).join("mcpmesh")
        );
        // No override → <base>\mcpmesh\<segments…>.
        assert_eq!(
            win_dir_from(None, Some(r"C:\Users\u\AppData\Local"), &["data"]).unwrap(),
            PathBuf::from(r"C:\Users\u\AppData\Local")
                .join("mcpmesh")
                .join("data")
        );
        // Missing/empty base env → typed NotFound error (parity with missing HOME).
        for base in [None, Some("")] {
            let err = win_dir_from(None, base, &[]).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        }
    }

    #[test]
    fn windows_pipe_name_is_per_user_and_env_isolatable() {
        // Stable per-user name from USERDOMAIN + USERNAME, sanitized + lowercased.
        assert_eq!(
            windows_pipe_name_from(Some("DESKTOP-AB12"), Some("Jo Hn"), None).unwrap(),
            PathBuf::from(r"\\.\pipe\mcpmesh-desktop-ab12-jo-hn")
        );
        // XDG_RUNTIME_DIR set (the family's test-isolation var) → a deterministic
        // suffix so hermetic tests get distinct pipes.
        let a = windows_pipe_name_from(Some("D"), Some("u"), Some(r"C:\tmp\t1")).unwrap();
        let b = windows_pipe_name_from(Some("D"), Some("u"), Some(r"C:\tmp\t2")).unwrap();
        assert_ne!(a, b);
        assert!(a.to_string_lossy().starts_with(r"\\.\pipe\mcpmesh-d-u-"));
        // Same input → same name (deterministic across processes: FNV, not DefaultHasher).
        assert_eq!(
            windows_pipe_name_from(Some("D"), Some("u"), Some(r"C:\tmp\t1")).unwrap(),
            a
        );
        // Missing USERNAME → typed error, never a shared anonymous pipe name.
        assert!(windows_pipe_name_from(None, None, None).is_err());
    }
}
