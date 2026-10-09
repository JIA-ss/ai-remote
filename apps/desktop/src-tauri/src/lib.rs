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
use ar_core::Profile;
use ar_net::ipc::Conn;
use serde_json::{Value, json};
use tauri::{State, ipc::Channel};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

struct AppState {
    profile: Profile,
    /// The `ai-remote` executable that runs the agent when none is running.
    cli: PathBuf,
    feeds: Mutex<HashMap<u32, tokio::task::JoinHandle<()>>>,
    next_feed: AtomicU32,
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

/// This device's id, version and paired hosts.
#[tauri::command]
#[specta::specta]
async fn agent_status(state: State<'_, AppState>) -> Result<Json, String> {
    state.request(json!({"op": "status"})).await
}

/// Paired hosts with the level granted at pairing and the current path (direct or relay).
#[tauri::command]
#[specta::specta]
async fn list_hosts(state: State<'_, AppState>) -> Result<Json, String> {
    state.request(json!({"op": "hosts"})).await
}

/// Redeems a host's pairing ticket; returns the host's name, the safety code and the level.
#[tauri::command]
#[specta::specta]
async fn pair(
    state: State<'_, AppState>,
    ticket: String,
    name: Option<String>,
) -> Result<Json, String> {
    state
        .request(json!({"op": "pair", "ticket": ticket, "name": name}))
        .await
}

/// Forgets a host on this device.
#[tauri::command]
#[specta::specta]
async fn unpair(state: State<'_, AppState>, host: String) -> Result<Json, String> {
    state.request(json!({"op": "unpair", "host": host})).await
}

/// Calls one host tool and returns the MCP result (`content`, `structuredContent`, `isError`).
#[tauri::command]
#[specta::specta]
async fn call_tool(
    state: State<'_, AppState>,
    host: Option<String>,
    tool: String,
    args: Json,
) -> Result<Json, String> {
    let args = match args.0 {
        Value::Null => json!({}),
        object @ Value::Object(_) => object,
        _ => return Err("tool arguments must be a JSON object".into()),
    };
    state
        .request(json!({"op": "call", "host": host, "tool": tool, "args": args}))
        .await
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
    conn.request(&json!({"op": "events", "host": host}))
        .await
        .map_err(text)?;
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
        list_hosts,
        pair,
        unpair,
        call_tool,
        subscribe_events,
        unsubscribe_events,
    ])
}

/// The `ai-remote` CLI that starts the controller agent: `AI_REMOTE_CLI`, else next to this app
/// (where installers and the bundled sidecar put it), else on PATH.
fn find_cli() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AI_REMOTE_CLI").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let name = if cfg!(windows) {
        "ai-remote.exe"
    } else {
        "ai-remote"
    };
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
        .ok_or_else(|| {
            anyhow!(
                "cannot find the ai-remote command next to this app or on PATH (set AI_REMOTE_CLI)"
            )
        })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = builder();
    #[cfg(debug_assertions)]
    builder
        .export(
            specta_typescript::Typescript::default(),
            "../src/bindings.ts",
        )
        .expect("export TypeScript bindings");

    let profile = Profile::open_default().expect("open the ai-remote profile");
    let cli = find_cli().unwrap_or_else(|e| {
        eprintln!("{e:#}");
        PathBuf::from("ai-remote")
    });
    tauri::Builder::default()
        .manage(AppState {
            profile,
            cli,
            feeds: Mutex::new(HashMap::new()),
            next_feed: AtomicU32::new(1),
        })
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the ai-remote app");
}

#[cfg(test)]
mod tests {
    /// Regenerates `src/bindings.ts` (also done at every debug start) and fails if a command's
    /// types cannot be exported.
    #[test]
    fn export_typescript_bindings() {
        super::builder()
            .export(
                specta_typescript::Typescript::default(),
                "../src/bindings.ts",
            )
            .expect("export TypeScript bindings");
    }
}
