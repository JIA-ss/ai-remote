//! iPhone controller: the same controller agent, inside the app, with no child processes.
//! No filesystem socket is needed. All remote operations still pass the host's gate.

use ar_core::Profile;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MobileError {
    #[error("{message}")]
    Failed { message: String },
}
type Result<T> = std::result::Result<T, MobileError>;
fn failure(e: impl std::fmt::Display) -> MobileError {
    MobileError::Failed { message: e.to_string() }
}

#[uniffi::export(with_foreign)]
pub trait MobileStream: Send + Sync {
    /// JSON activity events; callbacks are delivered off the UI thread.
    fn event(&self, json: String);
    /// One complete screen/1 message with its 18-byte header: a video frame (kind 1; the codec
    /// is in the flags: H.264 or HEVC Annex B), a pointer position (kind 4) or a pointer picture
    /// (kind 5), as the host's stream protocol documents them.
    fn frame(&self, bytes: Vec<u8>);
    fn ended(&self, reason: String);
}

#[derive(uniffi::Record)]
pub struct MobileView {
    pub subscription: u64,
    pub reply_json: String,
}

struct Subscription {
    cancel: CancellationToken,
    task: JoinHandle<()>,
    keyframes: Option<tokio::sync::mpsc::Sender<()>>,
    /// Command lines for a coding-agent chat (`chat_send`).
    commands: Option<tokio::sync::mpsc::Sender<String>>,
}

#[derive(uniffi::Object)]
pub struct MobileController {
    backend: ar_controller::EmbeddedController,
    closed: AtomicBool,
    next: AtomicU64,
    subscriptions: Mutex<HashMap<u64, Subscription>>,
}

#[uniffi::export(async_runtime = "tokio")]
impl MobileController {
    #[uniffi::constructor]
    pub async fn connect(directory: String) -> Result<Arc<Self>> {
        let profile = Profile::at(directory).map_err(failure)?;
        let backend = ar_controller::EmbeddedController::start(&profile).await.map_err(failure)?;
        Ok(Arc::new(Self {
            backend,
            closed: AtomicBool::new(false),
            next: AtomicU64::new(1),
            subscriptions: Mutex::new(HashMap::new()),
        }))
    }

    /// The native UI calls the canonical agent API. This never starts a CLI executable.
    pub async fn request(&self, request_json: String) -> Result<String> {
        let request: Value = serde_json::from_str(&request_json).map_err(failure)?;
        if !matches!(request["op"].as_str(), Some("status" | "hosts" | "pair" | "unpair" | "call" | "channel")) {
            return Err(failure("unsupported mobile controller request"));
        }
        self.ready()?;
        // Swift cancellation does not currently cancel UniFFI futures. Bound hand-back in Rust
        // so a phone background transition cannot wait indefinitely for an unreachable host.
        // chat_start waits for the host's owner to allow it (approval_timeout_secs, 60 s by default).
        let seconds = if request["request"]["turn"]["action"] == "hand_back" {
            3
        } else if request["tool"] == "chat_start" {
            330
        } else {
            30
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(seconds), self.backend.request(request))
            .await
            .map_err(|_| failure("request timed out; check the host and connection before retrying"))?
            .map_err(|e| failure(format!("{e:#}")))?;
        Ok(result.to_string())
    }

    pub async fn watch_events(&self, host: String, listener: Arc<dyn MobileStream>) -> Result<u64> {
        self.ready()?;
        let (stream, _) = self.backend.open_stream(json!({"op":"events", "host":host})).await.map_err(failure)?;
        let cancel = CancellationToken::new();
        let stopped = cancel.clone();
        let task = tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            let reason = loop {
                tokio::select! {
                    _ = stopped.cancelled() => break "stopped".to_string(),
                    line = lines.next_line() => match line {
                        Ok(Some(line)) => listener.event(line),
                        Ok(None) => break "host disconnected".to_string(),
                        Err(e) => break e.to_string(),
                    }
                }
            };
            listener.ended(reason);
        });
        self.register(Subscription { cancel, task, keyframes: None, commands: None })
    }

    pub async fn watch_screen(
        &self,
        host: String,
        monitor: Option<u32>,
        max_fps: u32,
        max_long_edge: u32,
        listener: Arc<dyn MobileStream>,
    ) -> Result<MobileView> {
        self.ready()?;
        let (stream, reply) = self
            .backend
            .open_stream(json!({"op":"screen", "host":host, "request": {
                "monitor":monitor, "max_fps":max_fps.clamp(1,30), "max_long_edge":max_long_edge.clamp(320,1920),
                // Every iPhone/iPad decodes HEVC in hardware; H.264 is the fallback the host always has.
                "codecs":["hevc","h264"],
                // The pointer comes separately (drawn by the app) where the host can do that.
                "cursor":"client"
            }}))
            .await
            .map_err(failure)?;
        let (read, mut write) = tokio::io::split(stream);
        let cancel = CancellationToken::new();
        let stopped = cancel.clone();
        let (keyframes, mut keys) = tokio::sync::mpsc::channel::<()>(4);
        let task = tokio::spawn(async move {
            let mut read = read;
            let frames = async {
                loop {
                    let mut header = [0u8; 18];
                    read.read_exact(&mut header).await.map_err(failure)?;
                    let size = u32::from_be_bytes(header[14..18].try_into().unwrap()) as usize;
                    if !matches!(header[0], 1 | 3 | 4 | 5) || size == 0 || size > 8 * 1024 * 1024 {
                        return Err::<(), MobileError>(failure("invalid screen frame header"));
                    }
                    let mut frame = Vec::with_capacity(18 + size);
                    frame.extend_from_slice(&header);
                    frame.resize(18 + size, 0);
                    read.read_exact(&mut frame[18..]).await.map_err(failure)?;
                    // Pong replies (kind 3) answer pings this client never sends.
                    if header[0] != 3 {
                        listener.frame(frame);
                    }
                }
            };
            let controls = async {
                while keys.recv().await.is_some() {
                    write.write_all(b"{\"keyframe\":true}\n").await.map_err(failure)?;
                }
                Ok::<(), MobileError>(())
            };
            let reason = tokio::select! {
                _ = stopped.cancelled() => "stopped".to_string(),
                r = frames => r.err().map_or_else(|| "host disconnected".to_string(), |e| e.to_string()),
                r = controls => r.err().map_or_else(|| "stopped".to_string(), |e| e.to_string()),
            };
            listener.ended(reason);
        });
        let subscription = self.register(Subscription { cancel, task, keyframes: Some(keyframes), commands: None })?;
        Ok(MobileView { subscription, reply_json: reply.to_string() })
    }

    pub async fn request_keyframe(&self, subscription: u64) {
        let sender = self.subscriptions.lock().unwrap().get(&subscription).and_then(|s| s.keyframes.clone());
        if let Some(sender) = sender {
            let _ = sender.try_send(());
        }
    }

    /// Opens a coding-agent chat with the ticket from a `chat_start` or `chat_attach` call. The reply is
    /// the host's first line (session, role, runtime, state, seq); every event after `since` then
    /// arrives as one JSON line through `listener.event`, as the host's chat protocol documents them.
    pub async fn watch_chat(
        &self,
        host: String,
        ticket: String,
        since: u64,
        listener: Arc<dyn MobileStream>,
    ) -> Result<MobileView> {
        self.ready()?;
        let (stream, reply) = self
            .backend
            .open_stream(json!({"op":"chat", "host":host, "request": {"ticket":ticket, "since":since}}))
            .await
            .map_err(failure)?;
        let (read, mut write) = tokio::io::split(stream);
        let cancel = CancellationToken::new();
        let stopped = cancel.clone();
        let (commands, mut lines_out) = tokio::sync::mpsc::channel::<String>(32);
        let task = tokio::spawn(async move {
            let events = async {
                let mut lines = BufReader::new(read).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) if !line.is_empty() => listener.event(line),
                        Ok(Some(_)) => {}
                        Ok(None) => return "host disconnected".to_string(),
                        Err(e) => return e.to_string(),
                    }
                }
            };
            let send = async {
                while let Some(line) = lines_out.recv().await {
                    write.write_all(line.as_bytes()).await.map_err(failure)?;
                }
                Ok::<_, MobileError>(write)
            };
            let reason = tokio::select! {
                _ = stopped.cancelled() => "stopped".to_string(),
                r = events => r,
                r = send => match r {
                    // Every sender is gone: the subscription was dropped without a stop.
                    Ok(mut write) => {
                        let _ = write.write_all(b"{\"bye\":{}}\n").await;
                        "stopped".to_string()
                    }
                    Err(e) => e.to_string(),
                },
            };
            listener.ended(reason);
        });
        let subscription = self.register(Subscription { cancel, task, keyframes: None, commands: Some(commands) })?;
        Ok(MobileView { subscription, reply_json: reply.to_string() })
    }

    /// Sends one command to an open chat: a JSON object such as `{"input":{"id":"..","text":".."}}`,
    /// `{"interrupt":{}}` or `{"answer":{"qid":"..","text":".."}}`. Commands go out in order.
    pub async fn chat_send(&self, subscription: u64, command_json: String) -> Result<()> {
        let command: Value = serde_json::from_str(&command_json).map_err(failure)?;
        if !command.is_object() {
            return Err(failure("a chat command is a JSON object"));
        }
        let sender = self.subscriptions.lock().unwrap().get(&subscription).and_then(|s| s.commands.clone());
        let Some(sender) = sender else { return Err(failure("this chat is closed")) };
        // Re-encoded so a command is always exactly one line.
        sender.send(format!("{command}\n")).await.map_err(|_| failure("this chat is closed"))
    }

    pub async fn stop_subscription(&self, subscription: u64) {
        let owned = self.subscriptions.lock().unwrap().remove(&subscription);
        if let Some(owned) = owned {
            owned.cancel.cancel();
            let _ = owned.task.await;
        }
    }

    /// Call when entering the background. Ends views and connections; pairing data stays on disk.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let all: Vec<_> = self.subscriptions.lock().unwrap().drain().map(|(_, s)| s).collect();
        for subscription in &all {
            subscription.cancel.cancel();
        }
        for subscription in all {
            let _ = subscription.task.await;
        }
        let _ = self.backend.close().await;
    }
}

impl MobileController {
    fn ready(&self) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) { Err(failure("controller is closed")) } else { Ok(()) }
    }
    fn register(&self, subscription: Subscription) -> Result<u64> {
        let mut all = self.subscriptions.lock().unwrap();
        if self.closed.load(Ordering::SeqCst) {
            subscription.cancel.cancel();
            return Err(failure("controller is closed"));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        all.insert(id, subscription);
        Ok(id)
    }
}

impl Drop for MobileController {
    fn drop(&mut self) {
        for (_, subscription) in self.subscriptions.get_mut().unwrap().drain() {
            subscription.cancel.cancel();
        }
    }
}
