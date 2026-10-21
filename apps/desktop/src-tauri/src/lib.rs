//! The ai-remote desktop app: a controller for people. It talks to the same per-user controller
//! agent as the `ai-remote` CLI and `ai-remote mcp`, so the app, the CLI and every agent share one
//! identity, one set of pairings and one connection per host.
//!
//! The commands here are a thin, typed bridge over the agent's local requests; TypeScript bindings
//! are generated from them (`src/bindings.ts`) in debug builds.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use anyhow::{Context, Result, anyhow};
use ar_core::{Profile, Role};
use ar_net::ipc::Conn;
use serde_json::{Value, json};
use tauri::{Manager, State, ipc::Channel};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

mod host_local;
mod login_item;
mod transfer;
mod tray;

struct AppState {
    profile: Profile,
    /// The `ai-remote` executable that runs the agent when none is running.
    cli: PathBuf,
    feeds: Mutex<HashMap<u32, tokio::task::JoinHandle<()>>>,
    next_feed: AtomicU32,
    /// Live views: the task copying frames, and the writer that asks the host for keyframes.
    views: Mutex<HashMap<u32, View>>,
    /// Live input streams (while you drive): the task copying lines, and the writer to the host.
    inputs: Mutex<HashMap<u32, InputStream>>,
}

/// A live input stream: lines for the host go through an unbounded queue in the order the page
/// sent them; `queued` counts what has not been written yet.
struct InputStream {
    task: tokio::task::JoinHandle<()>,
    control: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    queued: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// Queued writes past which batches of nothing but pointer moves are dropped (the next move says
/// where the pointer is anyway); presses, releases and keys are never dropped.
const INPUT_BACKLOG: usize = 64;

struct View {
    task: tokio::task::JoinHandle<()>,
    control: tokio::sync::mpsc::Sender<Vec<u8>>,
}

/// One live-view frame for the page: the host's 18-byte header and the H.264 payload, sent as raw
/// bytes (an `ArrayBuffer` in the page), never as JSON.
#[derive(specta::Type)]
pub struct Frame(#[specta(type = specta_typescript::Unknown)] Vec<u8>);

impl tauri::ipc::IpcResponse for Frame {
    fn body(self) -> tauri::Result<tauri::ipc::InvokeResponseBody> {
        Ok(tauri::ipc::InvokeResponseBody::Raw(self.0))
    }
}

impl AppState {
    async fn agent(&self) -> Result<Conn> {
        ar_controller::connect_agent_with(&self.profile, &self.cli).await
    }

    async fn request(&self, req: Value) -> Result<Json, String> {
        let mut conn = self.agent().await.map_err(text)?;
        conn.request(&req).await.map(Json).map_err(text)
    }
}

/// Any JSON value; `unknown` in TypeScript, narrowed by the page. (Exporting `serde_json::Value`
/// itself sends the binding generator into endless recursion.)
#[derive(serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(transparent)]
pub struct Json(#[specta(type = specta_typescript::Unknown)] pub Value);

fn text(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// This device's id, version, paired hosts and the agents (MCP clients) connected now.
#[tauri::command]
#[specta::specta]
async fn agent_status(state: State<'_, AppState>) -> Result<Json, String> {
    let mut status = state.request(json!({"op": "status"})).await?;
    // Use the executable we actually launch and the same profile, including bundled macOS
    // sidecars and profiles outside the default directory. Client apps need not inherit PATH.
    status.0["mcp"] = json!({
        "command": state.cli,
        "args": ["--home", state.profile.root().to_string_lossy(), "mcp"],
    });
    Ok(status)
}

/// Opens the system's appearance settings (light and dark), which the app follows.
#[tauri::command]
#[specta::specta]
fn open_appearance_settings() -> Result<(), String> {
    #[cfg(windows)]
    let opened = std::process::Command::new("explorer").arg("ms-settings:colors").spawn();
    #[cfg(target_os = "macos")]
    let opened = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.Appearance-Settings.extension")
        .spawn();
    #[cfg(not(any(windows, target_os = "macos")))]
    let opened: std::io::Result<std::process::Child> = Err(std::io::Error::other("not on this system"));
    opened.map(|_| ()).map_err(|e| e.to_string())
}

/// Cuts off one connected agent (an id from `agent_status`); its MCP client sees the connection close.
#[tauri::command]
#[specta::specta]
async fn disconnect_agent(state: State<'_, AppState>, id: u32) -> Result<Json, String> {
    state.request(json!({"op": "disconnect_agent", "id": id})).await
}

/// Paired hosts with the level granted at pairing and the current path (direct or relay).
#[tauri::command]
#[specta::specta]
async fn list_hosts(state: State<'_, AppState>) -> Result<Json, String> {
    state.request(json!({"op": "hosts"})).await
}

/// Lets direct (P2P) connections in through the system firewall: removes this app's inbound block
/// rules and allows inbound UDP. Asks for an administrator's consent on Windows.
#[tauri::command]
#[specta::specta]
async fn fix_firewall(state: State<'_, AppState>) -> Result<Json, String> {
    state.request(json!({"op": "fix_firewall"})).await
}

/// Redeems a host's pairing ticket; returns the host's name, the safety code and the level.
#[tauri::command]
#[specta::specta]
async fn pair(state: State<'_, AppState>, ticket: String, name: Option<String>) -> Result<Json, String> {
    state.request(json!({"op": "pair", "ticket": ticket, "name": name})).await
}

/// Unpairs a host: asks it to drop this device first (when it can be reached), then forgets it here.
/// The reply's `host_forgot_this_device` says whether the host took part.
#[tauri::command]
#[specta::specta]
async fn unpair(state: State<'_, AppState>, host: String) -> Result<Json, String> {
    let left = match state.agent().await {
        Ok(mut conn) => ar_controller::approver::channel(&mut conn, Some(&host), json!({"leave": true})).await.is_ok(),
        Err(_) => false,
    };
    let mut reply = state.request(json!({"op": "unpair", "host": host})).await?;
    reply.0["host_forgot_this_device"] = left.into();
    Ok(reply)
}

/// What this device may do on `host`: its level, the standing rules that concern it, the paired
/// devices (all of them for an admin) and the tools on offer, built in and mounted.
#[tauri::command]
#[specta::specta]
async fn host_access(state: State<'_, AppState>, host: String) -> Result<Json, String> {
    let mut conn = state.agent().await.map_err(text)?;
    ar_controller::approver::channel(&mut conn, Some(&host), json!({"access": true})).await.map(Json).map_err(text)
}

/// Lowers this device's level on `host` (`observe` or `control`); raising it takes someone there.
#[tauri::command]
#[specta::specta]
async fn lower_level(state: State<'_, AppState>, host: String, level: String) -> Result<(), String> {
    let mut conn = state.agent().await.map_err(text)?;
    ar_controller::approver::channel(&mut conn, Some(&host), json!({"lower": {"level": level}})).await.map_err(text)?;
    Ok(())
}

/// This computer as a host: each controller paired with it and whether its owner trusts it with
/// everything (bypass: every tool, no approval, no deny rules). `None` when no host runs here for this
/// profile. Read and changed only over the host's local control channel, never from a controller.
#[tauri::command]
#[specta::specta]
async fn local_host_bypass(state: State<'_, AppState>) -> Result<Option<Json>, String> {
    let endpoint = state.profile.control_endpoint(Role::Host).map_err(text)?;
    let Ok(mut conn) = Conn::connect(&endpoint).await else { return Ok(None) };
    conn.request(&json!({"op": "bypass"})).await.map(|v| Some(Json(v))).map_err(text)
}

/// Turns bypass on or off for a controller paired with this computer (see `local_host_bypass`).
#[tauri::command]
#[specta::specta]
async fn set_local_host_bypass(state: State<'_, AppState>, peer: String, on: bool) -> Result<Json, String> {
    let endpoint = state.profile.control_endpoint(Role::Host).map_err(text)?;
    let mut conn =
        Conn::connect(&endpoint).await.map_err(|_| "ai-remote host is not running on this computer".to_string())?;
    conn.request(&json!({"op": "set_bypass", "peer": peer, "on": on})).await.map(Json).map_err(text)
}

/// This computer as a host, for the "This device" page: the running host's `status` (`null` when
/// none runs here) and, with `doctor`, the doctor report: the host's own while it runs (its session,
/// permissions and network), otherwise `ai-remote doctor` run from the command, which still knows
/// the autostart state. The report reads the autostart and firewall state (slow on Windows), so the
/// page asks for it on opening and after a change, and polls the status alone.
#[tauri::command]
#[specta::specta]
async fn local_host(state: State<'_, AppState>, doctor: bool) -> Result<Json, String> {
    let (status, unknown) = match host_local::probe(&state.profile).await {
        host_local::Probe::Running(v) => (Some(v), None),
        host_local::Probe::Stopped => (None, None),
        host_local::Probe::Unknown(why) => (None, Some(why)),
    };
    let doctor = match (&status, doctor) {
        (_, false) => None,
        (Some(_), true) => host_local::request(&state.profile, json!({"op": "doctor"})).await.ok().flatten(),
        (None, true) => host_local::cli_json(&state.cli, &["doctor"]).await.ok(),
    };
    Ok(Json(json!({"host": status, "unknown": unknown, "doctor": doctor, "profile": state.profile.root()})))
}

/// A one-time pairing ticket from the host here, at `level`, and the pair code (with its QR code) that
/// hands it out. Both are credentials: the page shows them once in its own window and this app never
/// logs them.
#[tauri::command]
#[specta::specta]
async fn local_host_pair_code(state: State<'_, AppState>, level: String) -> Result<Json, String> {
    let level: ar_core::Level = serde_json::from_value(Value::String(level)).map_err(|e| e.to_string())?;
    host_local::require(&state.profile, json!({"op": "pair_code", "level": level})).await.map(Json).map_err(text)
}

/// Stops answering the pair code shown by `local_host_pair_code` (its dialog was closed). The
/// ticket behind it stays valid until it expires, but nothing hands it out any more.
#[tauri::command]
#[specta::specta]
async fn local_host_withdraw_code(state: State<'_, AppState>) -> Result<Json, String> {
    host_local::require(&state.profile, json!({"op": "withdraw_code"})).await.map(Json).map_err(text)
}

/// Pauses the agent on this computer (every controller's calls are refused until resumed) or
/// resumes it. From here, at this computer; controllers have their own way (`set_turn`).
#[tauri::command]
#[specta::specta]
async fn local_host_pause(app: tauri::AppHandle, state: State<'_, AppState>, on: bool) -> Result<Json, String> {
    let r = host_local::require(&state.profile, json!({"op": "pause", "on": on})).await.map(Json).map_err(text);
    refresh_tray(&app);
    r
}

/// Stops the host here (`ai-remote stop-host`); the autostart entry starts it again at the next
/// login, `local_host_start` right away.
#[tauri::command]
#[specta::specta]
async fn local_host_stop(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Json, String> {
    let r = host_local::require(&state.profile, json!({"op": "shutdown"})).await.map(Json).map_err(text);
    refresh_tray(&app);
    r
}

/// Starts the installed background host (`ai-remote host start`).
#[tauri::command]
#[specta::specta]
async fn local_host_start(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Json, String> {
    let r = host_local::cli_json(&state.cli, &["host", "start"]).await.map(Json).map_err(text);
    refresh_tray(&app);
    r
}

/// The host's latest audit lines (newest last).
#[tauri::command]
#[specta::specta]
async fn local_host_recent(state: State<'_, AppState>, limit: u32) -> Result<Json, String> {
    host_local::require(&state.profile, json!({"op": "recent", "limit": limit})).await.map(Json).map_err(text)
}

/// Opens one System Settings pane (`screen_recording`, `accessibility`, `login_items`, ...); the
/// person changes the setting there, never this app.
#[tauri::command]
#[specta::specta]
fn open_system_settings(pane: String) -> Result<(), String> {
    host_local::open_settings_pane(&pane).map_err(text)
}

/// Shows the host's `host/ai-remote.toml` (the approval rules) in the file manager, or its folder
/// while the file does not exist yet.
#[tauri::command]
#[specta::specta]
fn reveal_profile(state: State<'_, AppState>) -> Result<(), String> {
    let file = state.profile.host_config_path().map_err(text)?;
    host_local::reveal(&file).map_err(text)
}

/// Removes a controller's pairing here (`ai-remote forget`), e.g. when its safety code did not match.
#[tauri::command]
#[specta::specta]
async fn local_host_forget(state: State<'_, AppState>, peer: String) -> Result<Json, String> {
    host_local::require(&state.profile, json!({"op": "forget", "peer": peer})).await.map(Json).map_err(text)
}

/// Keeps the window out of screen capture (where the system supports it) while it shows a pairing
/// ticket, so a connected controller's screenshots and live view do not carry it.
#[tauri::command]
#[specta::specta]
fn protect_window(window: tauri::WebviewWindow, on: bool) -> Result<(), String> {
    window.set_content_protected(on).map_err(|e| e.to_string())
}

/// Whether the app opens at login (in the tray, without its window).
#[tauri::command]
#[specta::specta]
fn app_login_item() -> Result<bool, String> {
    login_item::enabled().map_err(text)
}

/// Turns opening at login on or off for this copy of the app.
#[tauri::command]
#[specta::specta]
fn set_app_login_item(on: bool) -> Result<(), String> {
    login_item::set(on).map_err(text)?;
    tray::set_resident(on);
    Ok(())
}

/// The page shows the window after its first paint, unless the app started in the tray (`--hidden`,
/// from the login item).
#[tauri::command]
#[specta::specta]
fn started_hidden() -> bool {
    login_item::started_hidden()
}

/// The page's language, so the tray menu reads the same.
#[tauri::command]
#[specta::specta]
fn set_ui_language(app: tauri::AppHandle, lang: String) {
    if let Some(t) = app.try_state::<tray::Tray<tauri::Wry>>() {
        t.set_lang(tray::Lang::parse(&lang));
    }
}

/// The tray polls again right away, so it follows a change made from the page.
fn refresh_tray(app: &tauri::AppHandle) {
    if let Some(t) = app.try_state::<tray::Tray<tauri::Wry>>() {
        t.refresh();
    }
}

/// Calls one host tool and returns the MCP result (`content`, `structuredContent`, `isError`).
/// `person`: the person at this device makes the call (while they drive), so it runs after they
/// took over; everything else is treated like the agent.
#[tauri::command]
#[specta::specta]
async fn call_tool(
    state: State<'_, AppState>,
    host: Option<String>,
    tool: String,
    args: Json,
    person: bool,
) -> Result<Json, String> {
    let args = match args.0 {
        Value::Null => json!({}),
        object @ Value::Object(_) => object,
        _ => return Err("tool arguments must be a JSON object".into()),
    };
    state.request(json!({"op": "call", "host": host, "tool": tool, "args": args, "person": person})).await
}

/// Subscribes to a host's live activity feed. Every event (one parsed JSON object) is sent on
/// `on_event`; returns an id for [`unsubscribe_events`].
#[tauri::command]
#[specta::specta]
async fn subscribe_events(
    state: State<'_, AppState>,
    host: Option<String>,
    on_event: Channel<Json>,
) -> Result<u32, String> {
    let mut conn = state.agent().await.map_err(text)?;
    conn.request(&json!({"op": "events", "host": host})).await.map_err(text)?;
    let (stream, buffered) = conn.into_raw();
    let id = state.next_feed.fetch_add(1, Ordering::Relaxed);
    let task = tokio::spawn(async move {
        let mut lines = BufReader::new(std::io::Cursor::new(buffered).chain(stream)).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if on_event.send(Json(event)).is_err() {
                break;
            }
        }
        // Let the page know the feed ended (host stopped, connection lost).
        let _ = on_event.send(Json(json!({"kind": "closed"})));
    });
    state.feeds.lock().unwrap().insert(id, task);
    Ok(id)
}

/// A call waiting for a person on a host.
#[derive(serde::Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct PendingApproval {
    pub id: String,
    /// The device whose agent asked.
    pub peer: String,
    pub tool: String,
    pub intent: Option<String>,
    pub args_preview: String,
    /// When the host stops waiting (milliseconds since the Unix epoch).
    pub expires_ms: f64,
}

/// What is waiting for approval on a host (or the only paired host), and that host's name.
#[tauri::command]
#[specta::specta]
async fn pending_approvals(
    state: State<'_, AppState>,
    host: Option<String>,
) -> Result<(String, Vec<PendingApproval>), String> {
    let mut conn = state.agent().await.map_err(text)?;
    let (host, waiting) = ar_controller::approver::pending(&mut conn, host.as_deref()).await.map_err(text)?;
    let waiting = waiting
        .into_iter()
        .map(|w| PendingApproval {
            id: w.id,
            peer: w.peer,
            tool: w.tool,
            intent: w.intent,
            args_preview: w.args_preview,
            expires_ms: w.expires_ms as f64,
        })
        .collect();
    Ok((host, waiting))
}

/// Allows one waiting call. Windows Hello asks the person first (over this window); the host checks
/// the signature against this device's accepted approver key.
#[tauri::command]
#[specta::specta]
async fn approve(state: State<'_, AppState>, host: String, id: String) -> Result<(), String> {
    let mut conn = state.agent().await.map_err(text)?;
    let (host, waiting) = ar_controller::approver::find(&mut conn, Some(&host), &id).await.map_err(text)?;
    ar_controller::approver::allow(&mut conn, &state.profile, &host, &waiting).await.map_err(text)
}

/// Denies one waiting call; `note` is passed to the agent ("Tell the agent instead").
#[tauri::command]
#[specta::specta]
async fn deny(state: State<'_, AppState>, host: String, id: String, note: Option<String>) -> Result<(), String> {
    let mut conn = state.agent().await.map_err(text)?;
    ar_controller::approver::deny(&mut conn, &host, &id, note.as_deref()).await.map_err(text)
}

/// Whether `host` accepted this device's approver key: `null` when this device has none yet.
#[tauri::command]
#[specta::specta]
async fn approver_status(state: State<'_, AppState>, host: Option<String>) -> Result<Option<bool>, String> {
    let mut conn = state.agent().await.map_err(text)?;
    ar_controller::approver::accepted(&mut conn, &state.profile, host.as_deref()).await.map_err(text)
}

/// Creates this device's approver key if needed (Windows Hello confirms) and offers it to `host`;
/// resolves once someone at the host accepted or declined it.
#[tauri::command]
#[specta::specta]
async fn enroll_approver(state: State<'_, AppState>, host: String) -> Result<Json, String> {
    let mut conn = state.agent().await.map_err(text)?;
    ar_controller::approver::enroll(&mut conn, &state.profile, Some(&host)).await.map(Json).map_err(text)
}

/// Changes who drives `host`: `take_over` (with `agentMayWatch`), `hand_back` (with an optional
/// note for the agent), `pause` or `resume`. Returns the new driver.
#[tauri::command]
#[specta::specta]
async fn set_turn(
    state: State<'_, AppState>,
    host: String,
    action: String,
    agent_may_watch: bool,
    note: Option<String>,
) -> Result<Json, String> {
    let request = json!({"turn": {"action": action, "agent_may_watch": agent_may_watch, "note": note}});
    let mut conn = state.agent().await.map_err(text)?;
    ar_controller::approver::channel(&mut conn, Some(&host), request).await.map(Json).map_err(text)
}

/// Leaves a message for the agent working on `host`; it arrives with its next call result.
#[tauri::command]
#[specta::specta]
async fn tell_agent(state: State<'_, AppState>, host: String, message: String) -> Result<(), String> {
    let mut conn = state.agent().await.map_err(text)?;
    ar_controller::approver::channel(&mut conn, Some(&host), json!({"tell": {"text": message}})).await.map_err(text)?;
    Ok(())
}

/// Starts a live view of `host`: frames go to `on_frame`; returns the view's id and the host's
/// monitors. `request` holds `monitor`, `max_fps`, `max_long_edge`, `bitrate_kbps` (all optional).
#[tauri::command]
#[specta::specta]
async fn watch_screen(
    state: State<'_, AppState>,
    host: Option<String>,
    request: Json,
    on_frame: Channel<Frame>,
) -> Result<(u32, Json), String> {
    use tokio::io::AsyncWriteExt;
    let mut conn = state.agent().await.map_err(text)?;
    let reply = conn.request(&json!({"op": "screen", "host": host, "request": request.0})).await.map_err(text)?;
    let (stream, buffered) = conn.into_raw();
    let (read, mut write) = tokio::io::split(stream);
    let (control, mut lines) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
    let id = state.next_feed.fetch_add(1, Ordering::Relaxed);
    // Measurement only: AI_REMOTE_TRACE_HOPS=<dir> logs when each video frame was read from the agent
    // and handed to the page (tools/hops-report.py).
    let mut trace = std::env::var_os("AI_REMOTE_TRACE_HOPS")
        .and_then(|dir| std::fs::File::create(std::path::Path::new(&dir).join("native.tsv")).ok())
        .map(std::io::BufWriter::new);
    let task = tokio::spawn(async move {
        let mut read = std::io::Cursor::new(buffered).chain(read);
        let frames = async {
            #[cfg(debug_assertions)]
            let mut first = true;
            loop {
                let mut header = [0u8; 18];
                if read.read_exact(&mut header).await.is_err() {
                    break;
                }
                let len = u32::from_be_bytes([header[14], header[15], header[16], header[17]]) as usize;
                // Kind 1 is a video frame, kind 3 a host's reply to a ping (see `ping_screen`), 4 and 5
                // the pointer's position and shape (requested with `"cursor":"client"`).
                let known = match header[0] {
                    1 => len <= 8 * 1024 * 1024,
                    3 | 4 => len <= 4096,
                    5 => len <= 8 + 1024 * 2048 * 4,
                    _ => false,
                };
                if !known || len == 0 {
                    break;
                }
                let mut frame = Vec::with_capacity(18 + len);
                frame.extend_from_slice(&header);
                frame.resize(18 + len, 0);
                if read.read_exact(&mut frame[18..]).await.is_err() {
                    break;
                }
                let (ts, read_at) = (u64::from_be_bytes(header[6..14].try_into().unwrap()), wall_us());
                if on_frame.send(Frame(frame)).is_err() {
                    break;
                }
                if let Some(t) = trace.as_mut().filter(|_| header[0] == 1) {
                    use std::io::Write;
                    let _ = writeln!(t, "{ts}\t{read_at}\t{}", wall_us());
                }
                #[cfg(debug_assertions)]
                if first {
                    first = false;
                    eprintln!("[video] sent first frame: {len} bytes");
                }
            }
        };
        let up = async {
            while let Some(line) = lines.recv().await {
                if write.write_all(&line).await.is_err() {
                    break;
                }
            }
        };
        tokio::select! {
            _ = frames => {}
            _ = up => {}
        }
        // Local IPC terminal marker, separate from the screen/1 video wire protocol.
        let mut ended = vec![2];
        ended
            .extend_from_slice(b"The screen stream ended. Retry, or select a lower quality if the connection is slow.");
        let _ = on_frame.send(Frame(ended));
    });
    state.views.lock().unwrap().insert(id, View { task, control });
    Ok((id, Json(reply)))
}

/// Microseconds since the Unix epoch (for the hop trace).
fn wall_us() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_micros())
}

/// Asks the host for a keyframe on live view `id` (after a decoder error).
#[tauri::command]
#[specta::specta]
async fn request_keyframe(state: State<'_, AppState>, id: u32) -> Result<(), String> {
    let control = state.views.lock().unwrap().get(&id).map(|v| v.control.clone());
    if let Some(control) = control {
        let _ = control.send(b"{\"keyframe\":true}\n".to_vec()).await;
    }
    Ok(())
}

/// Sends ping `n` on live view `id`; the host answers with its stream clock (a kind-3 message on
/// the view's channel), which maps frame timestamps onto the page's clock.
#[tauri::command]
#[specta::specta]
async fn ping_screen(state: State<'_, AppState>, id: u32, n: u32) -> Result<(), String> {
    let control = state.views.lock().unwrap().get(&id).map(|v| v.control.clone());
    if let Some(control) = control {
        let _ = control.try_send(format!("{{\"ping\":{n}}}\n").into_bytes());
    }
    Ok(())
}

/// Changes the bitrate of live view `id` while it runs (the page adapting to the network).
#[tauri::command]
#[specta::specta]
async fn set_screen_bitrate(state: State<'_, AppState>, id: u32, kbps: u32) -> Result<(), String> {
    let control = state.views.lock().unwrap().get(&id).map(|v| v.control.clone());
    if let Some(control) = control {
        let _ = control.send(format!("{{\"bitrate_kbps\":{kbps}}}\n").into_bytes()).await;
    }
    Ok(())
}

/// Ends live view `id`.
#[tauri::command]
#[specta::specta]
async fn stop_screen(state: State<'_, AppState>, id: u32) -> Result<(), String> {
    if let Some(view) = state.views.lock().unwrap().remove(&id) {
        view.task.abort();
    }
    Ok(())
}

/// Opens a person's live input stream to `host` (the host's `input/1` channel). Every line the host
/// writes (clipboard changes, refused events, pauses) is sent on `on_line`; returns an id for
/// [`send_input`] and [`close_input`], and the host's reply (`os`, `clipboard`).
#[tauri::command]
#[specta::specta]
async fn open_input(
    state: State<'_, AppState>,
    host: Option<String>,
    request: Json,
    on_line: Channel<String>,
) -> Result<(u32, Json), String> {
    use tokio::io::AsyncWriteExt;
    let mut conn = state.agent().await.map_err(text)?;
    let reply = conn.request(&json!({"op": "input", "host": host, "request": request.0})).await.map_err(text)?;
    let (stream, buffered) = conn.into_raw();
    let (read, mut write) = tokio::io::split(stream);
    let (control, mut lines) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let queued = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let written = queued.clone();
    let id = state.next_feed.fetch_add(1, Ordering::Relaxed);
    let task = tokio::spawn(async move {
        let mut read = BufReader::new(std::io::Cursor::new(buffered).chain(read));
        let down = async {
            let mut line = String::new();
            loop {
                line.clear();
                match read.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if on_line.send(line.trim_end_matches(['\r', '\n']).to_string()).is_err() {
                            break;
                        }
                    }
                }
            }
        };
        let up = async {
            while let Some(data) = lines.recv().await {
                written.fetch_sub(1, Ordering::Relaxed);
                if write.write_all(&data).await.is_err() || write.flush().await.is_err() {
                    break;
                }
            }
        };
        tokio::select! {
            _ = down => {}
            _ = up => {}
        }
        // The page learns the end from the empty line it never otherwise receives.
        let _ = on_line.send(String::new());
    });
    state.inputs.lock().unwrap().insert(id, InputStream { task, control, queued });
    Ok((id, Json(reply)))
}

/// Whether `data` (newline-separated JSON lines) holds only pointer moves.
fn only_moves(data: &str) -> bool {
    data.lines().filter(|l| !l.is_empty()).all(|l| l.starts_with("{\"m\":"))
}

/// Writes `data` (one or more newline-terminated JSON lines, see the host's input protocol) on
/// live input `id`. Synchronous on purpose: such commands run one after another in the order the
/// page sent them, so a key's release can never overtake its press (async commands run
/// concurrently). Returns false when a batch of moves was dropped behind a slow host.
#[tauri::command]
#[specta::specta]
fn send_input(state: State<'_, AppState>, id: u32, data: String) -> Result<bool, String> {
    let inputs = state.inputs.lock().unwrap();
    let Some(input) = inputs.get(&id) else { return Err("no such live input".into()) };
    if input.queued.load(Ordering::Relaxed) >= INPUT_BACKLOG && only_moves(&data) {
        return Ok(false);
    }
    input.queued.fetch_add(1, Ordering::Relaxed);
    if input.control.send(data.into_bytes()).is_err() {
        input.queued.fetch_sub(1, Ordering::Relaxed);
        return Err("live input ended".into());
    }
    Ok(true)
}

/// Ends live input `id`; the host releases every key and button it holds.
#[tauri::command]
#[specta::specta]
async fn close_input(state: State<'_, AppState>, id: u32) -> Result<(), String> {
    if let Some(input) = state.inputs.lock().unwrap().remove(&id) {
        input.task.abort();
    }
    Ok(())
}

/// The text on this computer's clipboard (`None` when it holds none).
#[tauri::command]
#[specta::specta]
async fn clipboard_text() -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(|| {
        let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        match clipboard.get_text() {
            Ok(text) => Ok(Some(text)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Replaces this computer's clipboard with `text` (what the host's clipboard holds).
#[tauri::command]
#[specta::specta]
async fn set_clipboard_text(text: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        clipboard.set_text(text).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Files on this computer's clipboard (copied in the file manager), as paths.
#[tauri::command]
#[specta::specta]
async fn clipboard_files() -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(|| {
        let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        Ok(clipboard
            .get()
            .file_list()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Sends the file at `local` to `host`, into folder `remote_dir` (`~/Desktop` by default) under its
/// own name. Progress (`[done, total]` bytes) goes on `on_progress`.
#[tauri::command]
#[specta::specta]
async fn push_file(
    state: State<'_, AppState>,
    host: String,
    local: String,
    remote_dir: Option<String>,
    on_progress: Channel<(f64, f64)>,
) -> Result<Json, String> {
    let report = |done: u64, total: u64| {
        let _ = on_progress.send((done as f64, total as f64));
    };
    let dir = remote_dir.filter(|d| !d.trim().is_empty()).unwrap_or_else(|| "~/Desktop".into());
    transfer::push(&state.profile, &state.cli, &host, std::path::Path::new(&local), &dir, &report)
        .await
        .map(Json)
        .map_err(text)
}

/// Fetches `remote` from `host` into this computer's Downloads folder (or `local_dir`).
#[tauri::command]
#[specta::specta]
async fn pull_file(
    state: State<'_, AppState>,
    host: String,
    remote: String,
    local_dir: Option<String>,
    on_progress: Channel<(f64, f64)>,
) -> Result<Json, String> {
    let report = |done: u64, total: u64| {
        let _ = on_progress.send((done as f64, total as f64));
    };
    let dir = match local_dir.filter(|d| !d.trim().is_empty()) {
        Some(d) => PathBuf::from(d),
        None => directories::UserDirs::new()
            .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
            .ok_or_else(|| "this computer has no Downloads folder".to_string())?,
    };
    transfer::pull(&state.profile, &state.cli, &host, &remote, &dir, &report).await.map(Json).map_err(text)
}

/// Writes a message from the page (an error, a warning) to the app's log, so problems inside the
/// webview show up where the app's own messages go.
#[tauri::command]
#[specta::specta]
fn log_from_page(level: String, message: String) {
    let message: String = message.chars().take(2000).collect();
    eprintln!("[page {level}] {message}");
}

/// Ends a feed started by [`subscribe_events`].
#[tauri::command]
#[specta::specta]
async fn unsubscribe_events(state: State<'_, AppState>, id: u32) -> Result<(), String> {
    if let Some(task) = state.feeds.lock().unwrap().remove(&id) {
        task.abort();
    }
    Ok(())
}

fn builder() -> tauri_specta::Builder<tauri::Wry> {
    tauri_specta::Builder::<tauri::Wry>::new().commands(tauri_specta::collect_commands![
        agent_status,
        disconnect_agent,
        open_appearance_settings,
        list_hosts,
        fix_firewall,
        pair,
        unpair,
        call_tool,
        subscribe_events,
        unsubscribe_events,
        pending_approvals,
        approve,
        deny,
        approver_status,
        enroll_approver,
        set_turn,
        tell_agent,
        host_access,
        lower_level,
        local_host_bypass,
        set_local_host_bypass,
        local_host,
        local_host_pair_code,
        local_host_withdraw_code,
        local_host_pause,
        local_host_stop,
        local_host_start,
        local_host_recent,
        local_host_forget,
        protect_window,
        open_system_settings,
        reveal_profile,
        set_ui_language,
        app_login_item,
        set_app_login_item,
        started_hidden,
        watch_screen,
        request_keyframe,
        ping_screen,
        set_screen_bitrate,
        stop_screen,
        open_input,
        send_input,
        close_input,
        clipboard_text,
        set_clipboard_text,
        clipboard_files,
        push_file,
        pull_file,
        log_from_page,
    ])
}

/// The `ai-remote` CLI that starts the controller agent: `AI_REMOTE_CLI`, else next to this app
/// (where installers and the bundled sidecar put it), else on PATH.
fn find_cli() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AI_REMOTE_CLI").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let name = if cfg!(windows) { "ai-remote.exe" } else { "ai-remote" };
    let exe = std::env::current_exe().context("locate this app")?;
    if let Some(dir) = exe.parent() {
        let beside = dir.join(name);
        if beside.is_file() {
            return Ok(beside);
        }
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow!("cannot find the ai-remote command next to this app or on PATH (set AI_REMOTE_CLI)"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = builder();
    #[cfg(debug_assertions)]
    builder
        .export(specta_typescript::Typescript::default(), concat!(env!("CARGO_MANIFEST_DIR"), "/../src/bindings.ts"))
        .expect("export TypeScript bindings");

    let profile = Profile::open_default().expect("open the ai-remote profile");
    let cli = find_cli().unwrap_or_else(|e| {
        eprintln!("{e:#}");
        PathBuf::from("ai-remote")
    });
    let tray_profile = profile.clone();
    let tray_cli = cli.clone();
    tauri::Builder::default()
        // Windows: our page draws the 48 px title bar; the plugin adds the caption buttons and the
        // native hit-test that keeps Snap Layouts on the maximize button. The window is shown by
        // the page after its first paint (no white flash).
        .plugin(tauri_plugin_frame::FramePluginBuilder::new().auto_titlebar(true).titlebar_height(48).build())
        .manage(AppState {
            profile,
            cli,
            feeds: Mutex::new(HashMap::new()),
            next_feed: AtomicU32::new(1),
            views: Mutex::new(HashMap::new()),
            inputs: Mutex::new(HashMap::new()),
        })
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            // The menu bar / notification area icon that shows this computer's host; the app
            // stays there when the window is closed while a host runs here.
            match tray::install(app.handle(), tray_profile, tray_cli) {
                Ok(tray) => {
                    app.manage(tray);
                }
                Err(e) => eprintln!("no tray icon: {e:#}"),
            }
            // Reading it runs reg.exe on Windows: off the main thread.
            std::thread::spawn(|| tray::set_resident(login_item::enabled().unwrap_or(false)));
            if login_item::started_hidden() {
                tray::set_dock(app.handle(), false);
            }
            #[cfg(debug_assertions)]
            smoke(app);
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window leaves the app in the tray while this computer is a host (the
            // icon is how its owner sees and stops it) or the app opens at login; otherwise closing
            // quits as before.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && window.app_handle().try_state::<tray::Tray<tauri::Wry>>().is_some_and(|t| t.keeps_running())
            {
                api.prevent_close();
                let _ = window.hide();
                tray::set_dock(window.app_handle(), false);
                // A live view left open would keep streaming unseen: the page leaves it.
                let _ = tauri::Emitter::emit(window, "window-hidden", ());
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building the ai-remote app")
        .run(|app, event| {
            // macOS: a click on the Dock icon brings the hidden window back.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { has_visible_windows: false, .. } = event {
                tray::show_main(app, json!({}));
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (app, event);
        });
}

/// Debug builds: `AI_REMOTE_SMOKE=<png>` runs the app without ever showing it to the person at the
/// computer (off screen, not focusable, not in the taskbar), saves a picture of its window after
/// `AI_REMOTE_SMOKE_SECS` (default 8) and exits. Launch it with
/// `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--disable-features=CalculateNativeWinOcclusion` so the
/// webview keeps drawing while off screen.
#[cfg(debug_assertions)]
fn smoke(app: &tauri::App) {
    use tauri::Manager;
    let Some(out) = std::env::var_os("AI_REMOTE_SMOKE") else { return };
    let secs: u64 = std::env::var("AI_REMOTE_SMOKE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_focusable(false);
        let _ = w.set_skip_taskbar(true);
        if std::env::var("AI_REMOTE_SMOKE_VISIBLE").as_deref() == Ok("1") {
            let _ = w.show();
        } else {
            let _ = w.set_position(tauri::PhysicalPosition::new(-8000, -8000));
        }
        // `AI_REMOTE_SMOKE_QUALITY=ultra` picks a live-view quality first (a stream-quality id; run
        // with a throwaway `WEBVIEW2_USER_DATA_FOLDER` so the choice does not stick).
        // Measurement runs read the page's per-second live-video metrics from the log.
        let _ = w.eval("localStorage.setItem('ai-remote.video-trace', '1')");
        if std::env::var_os("AI_REMOTE_TRACE_HOPS").is_some() {
            let _ = w.eval("localStorage.setItem('ai-remote.hops', '1')");
        }
        if let Ok(quality) = std::env::var("AI_REMOTE_SMOKE_QUALITY") {
            let quality = quality.replace('\'', "");
            let _ = w.eval(format!("localStorage.setItem('ai-remote.stream-quality', '{quality}')"));
        }
        // `AI_REMOTE_SMOKE_QUERY=view=session&host=office-pc` opens a page directly.
        if let Ok(q) = std::env::var("AI_REMOTE_SMOKE_QUERY") {
            let q = q.replace('\'', "");
            let _ = w.eval(format!("if (!location.search) location.replace('?{q}')"));
        }
    }
    let handle = app.handle().clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(secs));
        if let Some(w) = handle.get_webview_window("main") {
            let _ = w.eval("window.__TAURI_INTERNALS__.invoke('log_from_page', { level: 'smoke', message: JSON.stringify({ secure: isSecureContext, decoder: typeof VideoDecoder, visibility: document.visibilityState, canvases: Array.from(document.querySelectorAll('canvas')).map(c => [c.width, c.height]), metrics: document.querySelector('.stream-metrics')?.textContent ?? null, metricsDetail: document.querySelector('.stream-metrics')?.title ?? null }) })");
            let _ = w.eval("window.__TAURI_INTERNALS__.invoke('agent_status').then(s => window.__TAURI_INTERNALS__.invoke('log_from_page', { level: 'mcp', message: JSON.stringify(s.mcp) }))");
            let visible = w.is_visible().unwrap_or(false);
            eprintln!("[smoke] window visible: {visible}, url: {:?}", w.url().map(|u| u.to_string()));
            if !visible {
                let _ = w.show();
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
        let picture = handle.get_webview_window("main").ok_or("no window".to_string()).and_then(|w| capture(&w));
        let code = match picture.and_then(|img| img.save(&out).map_err(|e| e.to_string())) {
            Ok(()) => {
                eprintln!("[smoke] saved {}", std::path::Path::new(&out).display());
                0
            }
            Err(e) => {
                eprintln!("[smoke] cannot capture the window: {e}");
                1
            }
        };
        handle.exit(code);
    });
}

/// The window's pixels, drawn by the window itself (works off screen).
#[cfg(all(debug_assertions, windows))]
fn capture(w: &tauri::WebviewWindow) -> Result<image::RgbaImage, String> {
    use windows::Win32::{
        Foundation::{HWND, RECT},
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
            DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
        },
        Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow},
        UI::WindowsAndMessaging::GetWindowRect,
    };
    let hwnd = HWND(w.hwnd().map_err(|e| e.to_string())?.0);
    unsafe {
        let mut r = RECT::default();
        GetWindowRect(hwnd, &mut r).map_err(|e| e.to_string())?;
        let (width, height) = ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, width as i32, height as i32);
        let old = SelectObject(dc, bmp.into());
        // PW_RENDERFULLCONTENT (2): include DirectComposition content such as the webview.
        let ok = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(2)).as_bool();
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (width * height * 4) as usize];
        GetDIBits(dc, bmp, 0, height, Some(px.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        SelectObject(dc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        if !ok {
            return Err("PrintWindow failed".into());
        }
        for p in px.chunks_mut(4) {
            p.swap(0, 2);
            p[3] = 255;
        }
        image::RgbaImage::from_raw(width, height, px).ok_or_else(|| "bad buffer".into())
    }
}

#[cfg(all(debug_assertions, target_os = "macos"))]
fn capture(w: &tauri::WebviewWindow) -> Result<image::RgbaImage, String> {
    use block2::RcBlock;
    use objc2::AnyThread;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
    use objc2_foundation::{NSDictionary, NSError};
    use objc2_web_kit::WKWebView;

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    // with_webview dispatches on the main thread; wait only on the smoke worker, never AppKit.
    w.with_webview(move |webview| unsafe {
        let view = &*webview.inner().cast::<WKWebView>();
        let done = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
            let result = (|| {
                if let Some(error) = error.as_ref() {
                    return Err(error.localizedDescription().to_string());
                }
                let image = image.as_ref().ok_or("WKWebView returned no snapshot")?;
                let tiff = image.TIFFRepresentation().ok_or("snapshot has no bitmap")?;
                let bitmap = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &tiff)
                    .ok_or("cannot decode snapshot bitmap")?;
                let png = bitmap
                    .representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
                    .ok_or("cannot encode snapshot PNG")?;
                Ok(png.to_vec())
            })();
            let _ = tx.send(result);
        });
        view.takeSnapshotWithConfiguration_completionHandler(None, &done);
    })
    .map_err(|e| e.to_string())?;
    let bytes = rx.recv_timeout(std::time::Duration::from_secs(15)).map_err(|e| e.to_string())??;
    image::load_from_memory(&bytes).map(|i| i.into_rgba8()).map_err(|e| e.to_string())
}

#[cfg(all(debug_assertions, not(any(windows, target_os = "macos"))))]
fn capture(_: &tauri::WebviewWindow) -> Result<image::RgbaImage, String> {
    Err("capture is only implemented on Windows".into())
}

#[cfg(test)]
mod tests {
    /// Regenerates `src/bindings.ts` (also done at every debug start) and fails if a command's
    /// types cannot be exported.
    #[test]
    fn only_batches_of_moves_may_be_dropped() {
        assert!(super::only_moves("{\"m\":[1,2]}\n{\"m\":[3,4]}\n"));
        assert!(!super::only_moves("{\"m\":[1,2]}\n{\"b\":\"left\",\"down\":false}\n"));
        assert!(!super::only_moves("{\"k\":\"a\",\"down\":false}\n"));
    }

    #[test]
    fn export_typescript_bindings() {
        super::builder()
            .export(
                specta_typescript::Typescript::default(),
                concat!(env!("CARGO_MANIFEST_DIR"), "/../src/bindings.ts"),
            )
            .expect("export TypeScript bindings");
    }
}
