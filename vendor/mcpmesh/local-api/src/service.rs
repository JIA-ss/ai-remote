//! The shared plugin-platform seam (`service` feature): everything a plugin daemon
//! (kb, loc, …) needs to face the platform, extracted from the kb/loc byte-duplicates so
//! each rule has ONE home:
//!
//! - UDS faces: [`ensure_private_dir`] + [`bind_uds`] + [`check_peer_uid`] (0700
//!   symlink-refused owned runtime dir, 0600 socket, same-uid gate). The mcpmesh daemon's
//!   own control socket (`cli/src/ipc.rs`) binds through the SAME rule.
//! - THE audience-authz expansion: [`peer_audiences`] — `groups ∪ {eid} ∪ {user_id}`,
//!   default-deny. The single implementation both kb and loc gate on.
//! - `[services.*]` self-registration: [`register_service`] (empty allowlist; failures
//!   logged, never silently swallowed).
//! - `*-local/1` JSON-RPC conventions: [`ok`]/[`err`]/[`reply`]/[`internal`], the strict
//!   [`required_string_array`] param parse, and [`people_from_status`].
//! - the `*-local/1` Hello first frame: [`send_hello`].
//!
//! Deliberately NOT here: mcpmesh control-endpoint resolution. That is the featureless
//! [`crate::paths`] rule ([`crate::paths::default_endpoint`]) — ONE home for daemon, CLI,
//! and plugins, correct on both platforms (a named pipe on windows, never a joined
//! filesystem path).
//!
//! Deliberately NOT extracted (KISS until a third plugin proves the abstraction): state
//! models, Paths structs, tool dispatch/specs, fan-out policy, and each plugin's MCP
//! session skeleton in `remote.rs`.
//!
//! [`ensure_private_dir`]: crate::service::ensure_private_dir
//! [`bind_uds`]: crate::service::bind_uds
//! [`check_peer_uid`]: crate::service::check_peer_uid
//! [`peer_audiences`]: crate::service::peer_audiences
//! [`register_service`]: crate::service::register_service
//! [`ok`]: crate::service::ok
//! [`err`]: crate::service::err
//! [`reply`]: crate::service::reply
//! [`internal`]: crate::service::internal
//! [`required_string_array`]: crate::service::required_string_array
//! [`people_from_status`]: crate::service::people_from_status
//! [`send_hello`]: crate::service::send_hello
use std::io;
use std::path::Path;

use serde_json::{Value, json};
use tokio::io::AsyncWrite;
// The UDS-face check is exercised by the unix test module below (`UnixStream::connect`);
// non-test code reaches these faces through `crate::transport`, so the import is test-only
// and unix-only (the windows transport has no UDS fixtures).
#[cfg(all(test, unix))]
use tokio::net::UnixStream;

use crate::client::{ClientError, connect_control};
use crate::codec::write_frame;
use crate::protocol::{BackendSpec, Hello, RegisterServiceParams, Request};

// ---------------------------------------------------------------------------------------
// UDS faces
// ---------------------------------------------------------------------------------------

// The implementation now lives in `crate::transport` (the platform local-endpoint seam):
// on unix it is the SAME hardened UDS rule, moved verbatim. These unix-native names remain
// the plugin API on unix — the private monorepo consumes
// `mcpmesh_local_api::service::{ensure_private_dir, bind_uds, check_peer_uid}`, so they are
// re-exported here with identical signatures. Unix-only: `bind_uds`/`check_peer_uid`/
// `ensure_private_dir` are the UDS hardening rule (0700 dir, 0600 socket, peer-euid
// gate) and have no meaning on Windows, where the pipe's owner-only DACL is the whole
// gate (see `transport::windows`). The plugin consumers (kb, loc) are unix today.
#[cfg(unix)]
pub use crate::transport::{bind_uds, check_peer_uid, ensure_private_dir};

// ---------------------------------------------------------------------------------------
// THE audience-authz expansion (default-deny)
// ---------------------------------------------------------------------------------------

/// `peer_audiences = peer.groups ∪ {peer.eid} ∪ {peer.user_id}` — THE ONE
/// implementation of the caller-audience expansion every plugin gates on (kb re-exports it
/// as `effective_audiences`). An absent/empty peer yields an EMPTY set — default deny.
///
/// Never trusts a self-asserted value: the whole peer object is the platform-injected,
/// forge-proof `_meta["mcpmesh/peer"]` (the mcpmesh daemon authoritatively OVERWRITES it, so
/// `groups`/`user_id` can't be caller-forged).
///
/// `user_id` is the person's self-sovereign id (`b64u:<user_pk>`, present once a device→user
/// binding is verified — pairing OR roster). Including it means content shared to a PERSON
/// reaches ALL their devices (each presents the same verified user_id under a distinct
/// nickname), whereas `eid` (the stable device principal) scopes to one device and `groups`
/// to a roster set — three legitimate granularities.
///
/// Re-keyed on stable identity in 0.8.0 (#38): the platform injection carries the device
/// `eid:` principal and the display `name` is NEVER an audience — the identity hardening
/// this doc long promised, landed here, once.
pub fn peer_audiences(peer: &Value) -> Vec<String> {
    // The expansion itself is THE shared `principal_set` (crate::principals — the flat
    // namespace, one implementation for the mesh allow check, this seam, and the blob-scope
    // gate); this fn only adapts the platform-injected peer JSON onto it.
    let groups: Vec<String> = peer
        .get("groups")
        .and_then(|g| g.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|g| g.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    crate::principal_set(
        peer.get("eid").and_then(|v| v.as_str()),
        peer.get("user_id").and_then(|v| v.as_str()),
        &groups,
    )
    .into_iter()
    .map(str::to_owned)
    .collect()
}

// ---------------------------------------------------------------------------------------
// [services.*] self-registration
// ---------------------------------------------------------------------------------------

/// Register (or idempotently update) `[services.<service_name>]` on the running mcpmesh
/// daemon: a SOCKET backend pointing at `backend_sock`, with an EMPTY allowlist — local-only
/// until the user explicitly grants a peer (reachability is a user grant; the
/// content itself is gated per-audience inside each plugin's service).
///
/// A failure is ALWAYS logged here (`tracing::warn`) before being returned, so a
/// daemon treating registration as best-effort (`let _ =` — the mcpmesh daemon may not be up
/// in a headless test) can never silently swallow it.
pub async fn register_service(
    control_sock: &Path,
    service_name: &str,
    backend_sock: &Path,
) -> Result<(), ClientError> {
    let result = async {
        let mut client = connect_control(control_sock).await?;
        client
            .request(Request::RegisterService(RegisterServiceParams {
                name: service_name.to_string(),
                backend: BackendSpec::Socket {
                    path: backend_sock.to_string_lossy().into_owned(),
                },
                allow: vec![],
                // This helper connects → registers → disconnects, so it MUST be persistent: an
                // ephemeral registration would be torn down the instant this connection closes.
                // Ephemeral (#36) is for embedders that hold a ControlClient open for the
                // service's lifetime (see ControlClient::register_service_with).
                ephemeral: false,
                rate_limit_per_min: None,
            }))
            .await?;
        Ok(())
    }
    .await;
    if let Err(e) = &result {
        tracing::warn!(
            service = service_name,
            control_sock = %control_sock.display(),
            error = %e,
            "mcpmesh service registration failed — service stays unregistered until the daemon restarts"
        );
    }
    result
}

// ---------------------------------------------------------------------------------------
// *-local/1 JSON-RPC conventions
// ---------------------------------------------------------------------------------------

/// JSON-RPC error code: invalid params. (An unknown method answers `-32601`, the standard
/// JSON-RPC code — see `docs/local-protocol.md` "Error codes".)
pub const ERR_PARAMS: i64 = -32602;
/// JSON-RPC error code: internal error.
pub const ERR_INTERNAL: i64 = -32603;

/// A JSON-RPC success frame (absent id → null, the notification-shaped degenerate case).
pub fn ok(id: Option<Value>, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id": id.unwrap_or(Value::Null),"result": result})
}

/// A JSON-RPC error frame.
pub fn err(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id": id.unwrap_or(Value::Null),"error":{"code":code,"message":message}})
}

/// Wrap a handler's `Result` into a JSON-RPC response frame (the `*-local/1` dispatch shape).
pub fn reply(id: Value, r: Result<Value, (i64, String)>) -> Value {
    match r {
        Ok(v) => json!({"jsonrpc":"2.0","id":id,"result":v}),
        Err((code, message)) => {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
        }
    }
}

/// Map an internal failure to `(ERR_INTERNAL, "internal error")`: log the detail locally,
/// NEVER echo it to the caller (a retriever IO error may embed a filesystem path — e.g. a
/// hashed audience dir — that must not reach a peer or even the owner surface).
pub fn internal(e: impl std::fmt::Display) -> (i64, String) {
    tracing::warn!(error = %e, "internal error (detail withheld from the caller)");
    (ERR_INTERNAL, "internal error".to_string())
}

/// STRICT `params[key]` string-array parse: the key must be present, an array, and every
/// element a string — anything else is `ERR_PARAMS`. Destructive setters (share lists) MUST
/// use this: a lenient `unwrap_or_default()` would read a malformed request as "share with
/// NOBODY" and persist `[]` — an accidental unshare-everyone.
pub fn required_string_array(params: &Value, key: &str) -> Result<Vec<String>, (i64, String)> {
    let arr = params
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or((ERR_PARAMS, format!("{key} (array of strings) is required")))?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or((ERR_PARAMS, format!("{key} must contain only strings")))
        })
        .collect()
}

/// Extract the friendly people directory from an mcpmesh `status` result (`share_targets`):
/// one entry per paired peer — the owner's nickname for it + its verified `user_id` (or
/// null). Pure over the JSON so it is unit-tested without a live mcpmesh. Surface-clean:
/// nickname + user_id only, never a transport id / service list.
pub fn people_from_status(status: &Value) -> Vec<Value> {
    status["peers"]
        .as_array()
        .map(|peers| {
            peers
                .iter()
                .filter_map(|p| {
                    let name = p["name"].as_str()?;
                    Some(json!({ "name": name, "user_id": p["user_id"].as_str() }))
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------
// The *-local/1 Hello first frame
// ---------------------------------------------------------------------------------------

/// Write the `*-local/N` Hello first frame (the shared handshake convention: every owner-face
/// server sends `{api, api_version, stack_version}` before anything else).
pub async fn send_hello<W: AsyncWrite + Unpin>(
    writer: &mut W,
    api: &str,
    api_version: &str,
    api_minor: u32,
    stack_version: &str,
) -> io::Result<()> {
    let hello = serde_json::to_value(Hello {
        api: api.into(),
        api_version: api_version.into(),
        api_minor,
        stack_version: stack_version.into(),
    })
    .expect("Hello serializes");
    write_frame(writer, &hello).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{FrameReader, Inbound, MAX_FRAME_BYTES};
    // Only the unix-gated `register_service` stub below uses these Hello constants.
    #[cfg(unix)]
    use crate::protocol::{API_NAME, API_VERSION};
    use serde_json::json;

    #[test]
    fn peer_audiences_is_groups_union_eid_union_user_id() {
        // The platform injection carries the stable device principal `eid:` alongside the
        // display `name` — and the nickname is NEVER an audience (#38).
        let peer = json!({
            "name": "bob-laptop",
            "eid": "eid:0707",
            "user_id": "b64u:BOB",
            "groups": ["eng", "ops"]
        });
        let mut a = peer_audiences(&peer);
        a.sort();
        assert_eq!(a, vec!["b64u:BOB", "eid:0707", "eng", "ops"]);
        assert!(
            !a.iter().any(|s| s == "bob-laptop"),
            "display nickname must never be an audience"
        );
        // DEFAULT-DENY: an absent/empty peer yields nothing.
        assert!(peer_audiences(&json!({})).is_empty());
        // Empty-string eid/user_id never become audiences, and a name alone grants NOTHING.
        assert_eq!(
            peer_audiences(&json!({"name":"bob","eid":"eid:0707","user_id":"","groups":[]})),
            vec!["eid:0707"]
        );
        assert!(
            peer_audiences(&json!({"name":"bob","eid":"","user_id":"","groups":[]})).is_empty()
        );
    }

    #[test]
    fn people_from_status_extracts_nickname_and_user_id() {
        let status = json!({"peers":[
            {"name":"bob","services":["kb"],"user_id":"b64u:CGnYVhFY"},
            {"name":"carol","services":[]}
        ]});
        assert_eq!(
            people_from_status(&status),
            vec![
                json!({"name":"bob","user_id":"b64u:CGnYVhFY"}),
                json!({"name":"carol","user_id":null}),
            ]
        );
        assert!(people_from_status(&json!({})).is_empty());
    }

    #[test]
    fn internal_error_does_not_echo_detail() {
        // A retriever IO error may embed a hashed audience-dir path — it must NOT reach a peer.
        let (code, msg) =
            internal("open /home/me/.local/share/kb/index/abc123def456/notes.jsonl: No such file");
        assert_eq!(code, ERR_INTERNAL);
        assert!(
            !msg.contains("/home/me"),
            "no filesystem path in the caller-visible message"
        );
        assert!(
            !msg.contains("index/"),
            "no index dir in the caller-visible message"
        );
        assert_eq!(msg, "internal error");
    }

    #[test]
    fn required_string_array_is_strict() {
        // Present + array of strings → the values.
        let ok_p = json!({"audiences": ["b64u:BOB", "eng"]});
        assert_eq!(
            required_string_array(&ok_p, "audiences").unwrap(),
            vec!["b64u:BOB".to_string(), "eng".to_string()]
        );
        // Empty array is a VALID explicit "share with nobody".
        assert_eq!(
            required_string_array(&json!({"audiences": []}), "audiences").unwrap(),
            Vec::<String>::new()
        );
        // Missing key, wrong type, or a non-string element → ERR_PARAMS (never an implicit []).
        for bad in [
            json!({}),
            json!({"audiences": "eng"}),
            json!({"audiences": 42}),
            json!({"audiences": ["eng", 7]}),
            json!({"audiences": null}),
        ] {
            let e = required_string_array(&bad, "audiences").unwrap_err();
            assert_eq!(e.0, ERR_PARAMS, "payload {bad} must be a params error");
        }
    }

    #[test]
    fn ok_err_and_reply_shape_json_rpc_frames() {
        let o = ok(Some(json!(1)), json!({"x": true}));
        assert_eq!(o["id"], 1);
        assert_eq!(o["result"]["x"], true);
        let e = err(None, ERR_PARAMS, "bad");
        assert_eq!(e["id"], Value::Null);
        assert_eq!(e["error"]["code"], ERR_PARAMS);
        let r = reply(json!(7), Err((ERR_INTERNAL, "internal error".into())));
        assert_eq!(r["error"]["code"], ERR_INTERNAL);
        assert_eq!(
            reply(json!(8), Ok(json!({"ok":true})))["result"]["ok"],
            true
        );
    }

    // Unix-only: exercises the UDS hardening rule (0600/0700 bits, symlink refusal,
    // peer-euid gate). The windows transport's equivalent guarantee is the owner-only
    // DACL, covered by tests in `transport::windows`.
    #[cfg(unix)]
    #[tokio::test]
    async fn bind_uds_forces_0600_socket_and_0700_parent() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("plug");
        // Pre-create the runtime dir LAX (0755) — bind_uds must tighten it (loc-L6).
        std::fs::create_dir_all(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sock = run.join("plug.sock");
        let _listener = bind_uds(&sock).unwrap();
        let dir_mode = std::fs::metadata(&run).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "runtime dir forced private");
        let sock_mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(sock_mode, 0o600, "socket is owner-only");
        // Re-bind over a stale socket file succeeds (crash recovery).
        drop(_listener);
        let _again = bind_uds(&sock).unwrap();
    }

    /// Hardening parity: a SYMLINKED runtime dir is refused before any chmod/bind — a
    /// planted `link -> dir` must never redirect the socket (same rule as the
    /// daemon control socket).
    #[cfg(unix)]
    #[tokio::test]
    async fn bind_uds_refuses_a_symlinked_runtime_dir() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let err = bind_uds(&link.join("plug.sock")).unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "refusal names the symlink: {err}"
        );
        // And ensure_private_dir itself refuses directly too.
        assert!(ensure_private_dir(&link).is_err());
        // The real dir still binds fine (the check refuses links, not dirs).
        let _ok = bind_uds(&real.join("plug.sock")).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn check_peer_uid_accepts_a_same_uid_peer() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("uid.sock");
        let listener = bind_uds(&sock).unwrap();
        let client = UnixStream::connect(&sock).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        // Both ends of a same-process connection are, by construction, the same uid.
        assert!(check_peer_uid(&server));
        assert!(check_peer_uid(&client));
    }

    #[tokio::test]
    async fn send_hello_writes_the_family_hello_frame() {
        let (mut a, b) = tokio::io::duplex(1024);
        send_hello(&mut a, "loc-local/1", "1", 0, "0.1.0")
            .await
            .unwrap();
        drop(a);
        let mut reader = FrameReader::new(b, MAX_FRAME_BYTES);
        let frame = match reader.next().await.unwrap().unwrap() {
            Inbound::Frame(v) => v,
            Inbound::Violation(v) => panic!("violation: {v:?}"),
        };
        assert_eq!(frame["api"], "loc-local/1");
        assert_eq!(frame["api_version"], "1");
        assert_eq!(frame["stack_version"], "0.1.0");
    }

    /// A stub mcpmesh daemon that answers one `register_service`, asserting the wire shape.
    /// Unix-only for now: the stub daemon binds a raw `UnixListener`; porting it to the
    /// transport seam would let it run on windows too.
    #[cfg(unix)]
    #[tokio::test]
    async fn register_service_registers_a_socket_backend_with_empty_allow() {
        let dir = tempfile::tempdir().unwrap();
        let control = dir.path().join("mcpmesh.sock");
        let listener = tokio::net::UnixListener::bind(&control).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut writer) = stream.into_split();
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
            assert_eq!(req["method"], "register_service");
            assert_eq!(req["params"]["name"], "loc");
            assert_eq!(
                req["params"]["backend"]["socket"]["path"],
                "/run/x/loc/loc.sock"
            );
            assert_eq!(req["params"]["allow"], json!([]));
            write_frame(
                &mut writer,
                &json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}}),
            )
            .await
            .unwrap();
        });
        register_service(&control, "loc", Path::new("/run/x/loc/loc.sock"))
            .await
            .unwrap();
        server.await.unwrap();

        // And the failure path returns Err (after logging) instead of swallowing (loc-L2).
        let gone = dir.path().join("nobody-home.sock");
        assert!(
            register_service(&gone, "loc", Path::new("/x"))
                .await
                .is_err()
        );
    }
}
