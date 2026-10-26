//! Coding agents for a host that runs as SYSTEM (Windows service mode): they must run as the
//! signed-in user, and SYSTEM must never execute the programs in that user's folders (that would
//! hand anyone who can write there SYSTEM's rights). So the host starts a **runner** in the user's
//! session, with the user's own token (`ai-remote-hostw --chat-runner`, see [`super::runner_win`]),
//! and the runner does everything that executes something of the user's: finding the agents
//! (`--version`, sign-in checks) and running them with their adapters. The host keeps what it
//! keeps for local agents: sessions, transcripts, numbering, queues, audit and every limit.
//!
//! The runner connects to the host's endpoint (a named pipe that admits SYSTEM and the installing
//! user, [`ar_net::ipc::Listener::bind_shared`]); the pipe's ACL is not what authenticates it.
//! Both sides prove they hold the runner's key, a random 32 bytes the host made for it and handed
//! over in its environment block when it started it, with a challenge in each direction
//! ([`handshake_host`], [`handshake_runner`]): a process that squats the pipe name learns nothing
//! and cannot have the user's agents run, and a process that connects without the key is refused.
//!
//! The runner is not in the host's job, so it outlives the host: Windows 11 ends the host when the
//! screen locks (the console moves to a new session and the service restarts the host there). The
//! agents keep running; the runner keeps what they report, numbered, until the host acknowledges
//! it. The restarted host finds the runner again (it reconnects), asks what is still running
//! ([`RunnerLink::lives`]) and takes each live agent back ([`RunnerLink::adopt`]) from the last
//! number it recorded, so a session goes on and viewers resume with `since` as after any break.
//!
//! The runner ends with its agents when the host tells it to (the host stopped cleanly) or when no
//! host has connected for a while ([`IDLE_EMPTY`], [`IDLE_BUSY`]).

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{mpsc, oneshot, watch},
};

use super::{
    Image,
    adapters::{self, Launch, LiveCmd, LiveHandle},
    detect::{self, Detector},
    event::Out,
    runtimes::CustomAgent,
};

/// The variable that carries the runner's key (hex) into the runner. The runner removes it from its
/// own environment at once, so its agents never see it.
pub const KEY_ENV: &str = "AI_REMOTE_CHAT_RUNNER_KEY";
/// Longest line either side accepts.
const MAX_LINE: u64 = 40 * 1024 * 1024;
/// How long the handshake may take.
const HANDSHAKE: Duration = Duration::from_secs(5);
/// How often a disconnected runner tries the host's endpoint again.
const RETRY: Duration = Duration::from_millis(250);
/// A runner with no agent running ends after this long without a host.
pub const IDLE_EMPTY: Duration = Duration::from_secs(2 * 60);
/// A runner with agents ends (stopping them) after this long without a host.
pub const IDLE_BUSY: Duration = Duration::from_secs(15 * 60);
/// What a runner keeps unacknowledged for one agent before it stops reading the agent's output
/// (the agent then waits on its pipe).
const MAX_UNACKED: usize = 16 * 1024 * 1024;
/// How long a restarted host waits for a runner that is still running to come back.
const RECONNECT_GRACE: Duration = Duration::from_secs(3);
/// How long a runner the host started may take to connect.
const START_WAIT: Duration = Duration::from_secs(20);
/// Answer time for requests that only look things up; detection runs probes of up to 10 s each.
const LOOKUP: Duration = Duration::from_secs(60);
/// How often forwarded output is recorded as passed on (see [`Out::Mark`]).
const MARK_EVERY: Duration = Duration::from_millis(500);

/// Makes a new runner key.
pub fn new_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    let _ = getrandom::fill(&mut k);
    k
}

pub fn key_hex(k: &[u8; 32]) -> String {
    data_encoding::HEXLOWER.encode(k)
}

pub fn parse_key(s: &str) -> Result<[u8; 32]> {
    let v = data_encoding::HEXLOWER_PERMISSIVE.decode(s.trim().as_bytes()).context("the runner key is not hex")?;
    v.try_into().map_err(|_| anyhow!("the runner key must be 32 bytes"))
}

/// The key kept in `path`, or a new one written there. A restarted host needs the key of the runner
/// that is still running.
pub fn load_or_create_key(path: &Path) -> Result<[u8; 32]> {
    if let Ok(text) = std::fs::read_to_string(path)
        && let Ok(k) = parse_key(&text)
    {
        return Ok(k);
    }
    let k = new_key();
    ar_core::store::write_atomic(path, key_hex(&k).as_bytes())?;
    ar_core::store::restrict_permissions(path);
    Ok(k)
}

fn mac(key: &[u8; 32], who: &str, a: &str, b: &str) -> blake3::Hash {
    blake3::keyed_hash(key, format!("ai-remote chat runner|{who}|{a}|{b}").as_bytes())
}

fn nonce() -> String {
    super::random_hex()
}

/// The stream of a connection, read line by line.
pub struct Wire<S> {
    io: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Wire<S> {
    pub fn new(s: S) -> Self {
        Self { io: BufReader::new(s) }
    }

    async fn read(&mut self) -> Result<Option<Value>> {
        read_line(&mut self.io).await
    }

    async fn write(&mut self, v: &Value) -> Result<()> {
        let mut b = serde_json::to_vec(v)?;
        b.push(b'\n');
        self.io.get_mut().write_all(&b).await?;
        self.io.get_mut().flush().await?;
        Ok(())
    }
}

async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Option<Value>> {
    let mut line = String::new();
    let n = (&mut *r).take(MAX_LINE).read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    if !line.ends_with('\n') && n as u64 >= MAX_LINE {
        bail!("line too long");
    }
    Ok(Some(serde_json::from_str(line.trim()).context("malformed line")?))
}

fn field<'a>(v: &'a Option<Value>, k: &str) -> Option<&'a str> {
    v.as_ref().and_then(|v| v.get(k)).and_then(Value::as_str)
}

/// The host's side of the handshake: the runner must prove it holds `key`, and learns that the
/// host does. Returns the runner's process id as it claims it (for the log).
pub async fn handshake_host<S: AsyncRead + AsyncWrite + Unpin>(w: &mut Wire<S>, key: &[u8; 32]) -> Result<u64> {
    tokio::time::timeout(HANDSHAKE, async {
        let hello = w.read().await?;
        let theirs = field(&hello, "nonce").filter(|n| n.len() == 32).ok_or_else(|| anyhow!("no hello"))?.to_string();
        let pid = hello.as_ref().and_then(|v| v["pid"].as_u64()).unwrap_or(0);
        let ours = nonce();
        w.write(&json!({"nonce": ours, "mac": mac(key, "host", &theirs, &ours).to_hex().as_str()})).await?;
        let proof = w.read().await?;
        let got = field(&proof, "mac").and_then(|m| blake3::Hash::from_hex(m).ok());
        // blake3::Hash compares in constant time.
        if got != Some(mac(key, "runner", &ours, &theirs)) {
            let _ = w.write(&json!({"ok": false})).await;
            bail!("the runner did not prove it holds the key");
        }
        w.write(&json!({"ok": true})).await?;
        Ok(pid)
    })
    .await
    .map_err(|_| anyhow!("the runner's handshake timed out"))?
}

/// The runner's side: the host must prove it holds `key` before the runner takes any request.
pub async fn handshake_runner<S: AsyncRead + AsyncWrite + Unpin>(w: &mut Wire<S>, key: &[u8; 32]) -> Result<()> {
    tokio::time::timeout(HANDSHAKE, async {
        let ours = nonce();
        w.write(&json!({"nonce": ours, "pid": std::process::id()})).await?;
        let challenge = w.read().await?;
        let theirs = field(&challenge, "nonce").filter(|n| n.len() == 32).ok_or_else(|| anyhow!("no challenge"))?;
        let got = field(&challenge, "mac").and_then(|m| blake3::Hash::from_hex(m).ok());
        if got != Some(mac(key, "host", &ours, theirs)) {
            bail!("the endpoint is not the host that started this runner (wrong key)");
        }
        let theirs = theirs.to_string();
        w.write(&json!({"mac": mac(key, "runner", &theirs, &ours).to_hex().as_str()})).await?;
        match w.read().await? {
            Some(v) if v["ok"] == true => Ok(()),
            _ => bail!("the host refused this runner"),
        }
    })
    .await
    .map_err(|_| anyhow!("the host's handshake timed out"))?
}

/// An agent the runner still runs, as it reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveInfo {
    pub live: String,
    pub pid: u32,
    pub start: u64,
    pub grace_ms: u64,
    /// A turn runs (a message went in and its end has not come out yet).
    pub busy: bool,
    /// The agent process is gone; what it reported last may still wait to be passed on.
    pub gone: bool,
}

/// What a runner says about an agent the host can start.
#[derive(Debug, Clone, Deserialize)]
pub struct Prepared {
    pub id: String,
    pub name: String,
    pub program: String,
    pub cwd: String,
}

// ---------------------------------------------------------------------------------------------
// Host side
// ---------------------------------------------------------------------------------------------

/// Starts a runner; returns when the process was started (not when it connected).
pub type Spawner = Box<dyn Fn() -> Result<()> + Send + Sync>;

/// The host's connection to its runner, or the lack of one: requests wait for a runner and start
/// one when none comes.
#[derive(Clone)]
pub struct RunnerLink(Arc<LinkInner>);

struct LinkInner {
    conn: Mutex<Option<Arc<Conn>>>,
    /// Bumped on every connect and disconnect.
    changed: watch::Sender<u64>,
    spawner: Spawner,
    /// Starting at most one runner at a time.
    starting: tokio::sync::Mutex<()>,
    /// Whether the start-up wait for a runner that outlived the previous host is over.
    settled: AtomicBool,
    next_id: AtomicU64,
}

struct Conn {
    tx: mpsc::UnboundedSender<String>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    lives: Mutex<HashMap<String, Sink>>,
}

/// Where the output of one agent goes: the session's adapter channel.
struct Sink {
    out: mpsc::Sender<Out>,
    /// The runner's number of the last output passed on.
    last: u64,
    /// The number last recorded with [`Out::Mark`].
    marked: u64,
}

impl RunnerLink {
    pub fn new(spawner: Spawner) -> Self {
        let (changed, _) = watch::channel(0);
        Self(Arc::new(LinkInner {
            conn: Mutex::new(None),
            changed,
            spawner,
            starting: tokio::sync::Mutex::new(()),
            settled: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
        }))
    }

    pub fn connected(&self) -> bool {
        self.0.conn.lock().unwrap().is_some()
    }

    /// Serves a connection whose handshake passed, until it ends. A second runner while one is
    /// connected is turned away.
    pub async fn serve<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(&self, wire: Wire<S>) {
        // Keep the handshake's reader: what the other side sent right after it may already be in
        // its buffer (a host that came back asks for the live agents at once).
        let (read, mut write) = tokio::io::split(wire.io);
        let mut read = BufReader::new(read);
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let conn = Arc::new(Conn { tx, pending: Mutex::new(HashMap::new()), lives: Mutex::new(HashMap::new()) });
        {
            let mut slot = self.0.conn.lock().unwrap();
            if slot.is_some() {
                tracing::warn!("a second agent runner connected while one is connected; it is turned away");
                return;
            }
            *slot = Some(conn.clone());
        }
        self.0.changed.send_modify(|g| *g += 1);
        let writer = tokio::spawn(async move {
            while let Some(mut line) = rx.recv().await {
                line.push('\n');
                if write.write_all(line.as_bytes()).await.is_err() || write.flush().await.is_err() {
                    break;
                }
            }
        });
        let mut tick = tokio::time::interval(MARK_EVERY);
        loop {
            tokio::select! {
                line = read_line(&mut read) => match line {
                    Ok(Some(v)) => self.incoming(&conn, v).await,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!("agent runner connection: {e:#}");
                        break;
                    }
                },
                _ = tick.tick() => mark(&conn).await,
            }
        }
        writer.abort();
        *self.0.conn.lock().unwrap() = None;
        self.0.changed.send_modify(|g| *g += 1);
        for (_, tx) in conn.pending.lock().unwrap().drain() {
            let _ = tx.send(Err("the agent runner went away".into()));
        }
        let sinks: Vec<_> = conn.lives.lock().unwrap().drain().map(|(_, s)| s.out).collect();
        for out in sinks {
            let _ = out.send(Out::Exited("the agent runner in the user's session stopped".into())).await;
        }
        tracing::info!("the agent runner disconnected");
    }

    async fn incoming(&self, conn: &Conn, v: Value) {
        if let Some(id) = v["id"].as_u64() {
            let reply = match v.get("err").and_then(Value::as_str) {
                Some(e) => Err(e.to_string()),
                None => Ok(v.get("ok").cloned().unwrap_or(Value::Null)),
            };
            if let Some(tx) = conn.pending.lock().unwrap().remove(&id) {
                let _ = tx.send(reply);
            }
            return;
        }
        if v["op"] != "out" {
            return;
        }
        let (Some(live), Some(n)) = (v["live"].as_str(), v["n"].as_u64()) else { return };
        let Ok(out) = serde_json::from_value::<Out>(v["out"].clone()) else {
            tracing::warn!(live, "the agent runner sent output this host does not understand");
            return;
        };
        let ends = matches!(out, Out::TurnEnd { .. } | Out::Exited(_));
        let tx = {
            let mut lives = conn.lives.lock().unwrap();
            let Some(s) = lives.get_mut(live) else { return };
            if n <= s.last {
                return; // sent again after a reconnection and already passed on
            }
            s.last = n;
            s.out.clone()
        };
        let _ = tx.send(out).await;
        if ends {
            mark(conn).await;
        }
    }

    /// Waits until a runner is connected, starting one when none comes.
    async fn ensure(&self) -> Result<Arc<Conn>> {
        if let Some(c) = self.conn() {
            return Ok(c);
        }
        let _one = self.0.starting.lock().await;
        if !self.0.settled.load(Ordering::SeqCst) {
            // A runner that outlived the previous host reconnects within a fraction of a second.
            if let Some(c) = self.wait(RECONNECT_GRACE).await {
                return Ok(c);
            }
            self.0.settled.store(true, Ordering::SeqCst);
        }
        if let Some(c) = self.conn() {
            return Ok(c);
        }
        (self.0.spawner)().context("cannot start the agent runner in the user's session")?;
        self.wait(START_WAIT).await.ok_or_else(|| {
            anyhow!(
                "the agent runner started in the user's session did not connect within {} s (see its log, \
                 host/chat-runner.log)",
                START_WAIT.as_secs()
            )
        })
    }

    fn conn(&self) -> Option<Arc<Conn>> {
        self.0.conn.lock().unwrap().clone()
    }

    async fn wait(&self, d: Duration) -> Option<Arc<Conn>> {
        let mut rx = self.0.changed.subscribe();
        let deadline = tokio::time::Instant::now() + d;
        loop {
            if let Some(c) = self.conn() {
                return Some(c);
            }
            if tokio::time::timeout_at(deadline, rx.changed()).await.is_err() {
                return None;
            }
        }
    }

    async fn request(&self, conn: &Conn, mut req: Value, timeout: Duration) -> Result<Value> {
        let id = self.0.next_id.fetch_add(1, Ordering::SeqCst);
        req["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        conn.pending.lock().unwrap().insert(id, tx);
        if conn.tx.send(req.to_string()).is_err() {
            conn.pending.lock().unwrap().remove(&id);
            bail!("the agent runner went away");
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(anyhow!("{e}")),
            Ok(Err(_)) => bail!("the agent runner went away"),
            Err(_) => {
                conn.pending.lock().unwrap().remove(&id);
                bail!("the agent runner did not answer within {} s", timeout.as_secs())
            }
        }
    }

    /// The installed agents, as the runner finds them with the user's rights.
    pub async fn runtimes(&self, custom: &[CustomAgent], refresh: bool) -> Result<Value> {
        let c = self.ensure().await?;
        self.request(&c, json!({"op": "runtimes", "custom": custom, "refresh": refresh}), LOOKUP).await
    }

    /// The bytes of a file for a preview, read by the runner with the user's rights.
    pub async fn read(&self, path: &Path) -> Result<Vec<u8>> {
        let c = self.ensure().await?;
        let v = self.request(&c, json!({"op": "read", "path": path.display().to_string()}), LOOKUP).await?;
        BASE64.decode(v["data"].as_str().context("the runner sent no data")?).context("the runner sent bad data")
    }

    /// Checks that `runtime` can start in `cwd` (expanded and checked as the user).
    pub async fn prepare(&self, runtime: &str, custom: &[CustomAgent], cwd: &str) -> Result<Prepared> {
        let c = self.ensure().await?;
        let v = self
            .request(&c, json!({"op": "prepare", "runtime": runtime, "custom": custom, "cwd": cwd}), LOOKUP)
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Starts an agent in the runner as the live agent `live`.
    #[allow(clippy::too_many_arguments)]
    pub async fn launch(
        &self,
        live: &str,
        runtime: &str,
        custom: &[CustomAgent],
        cwd: &str,
        model: Option<&str>,
        hardened: bool,
        resume: Option<&str>,
        fork: bool,
        permission_mode: Option<&str>,
    ) -> Result<(LiveHandle, mpsc::Receiver<Out>)> {
        let c = self.ensure().await?;
        let (out_tx, out) = mpsc::channel(1024);
        c.lives.lock().unwrap().insert(live.to_string(), Sink { out: out_tx, last: 0, marked: 0 });
        // A fork names the conversation as `fork_from`, not `resume`: a runner from before forks
        // ignores the field and starts a new conversation, instead of continuing the other session's.
        let (resume, fork_from) = if fork { (None, resume) } else { (resume, None) };
        let req = json!({
            "op": "launch", "live": live, "runtime": runtime, "custom": custom, "cwd": cwd, "model": model,
            "hardened": hardened, "resume": resume, "fork_from": fork_from, "permission_mode": permission_mode,
        });
        let info = match self.request(&c, req, LOOKUP).await.and_then(|v| Ok(serde_json::from_value::<LiveInfo>(v)?)) {
            Ok(i) => i,
            Err(e) => {
                c.lives.lock().unwrap().remove(live);
                return Err(e);
            }
        };
        Ok((self.handle(c, info), out))
    }

    /// The agents the runner still runs. Waits briefly for a runner that outlived the previous
    /// host, never starts one: a runner that is not running has nothing to adopt.
    pub async fn lives(&self) -> Vec<LiveInfo> {
        let conn = match self.conn() {
            Some(c) => Some(c),
            None => self.wait(RECONNECT_GRACE).await,
        };
        self.0.settled.store(true, Ordering::SeqCst);
        let Some(c) = conn else { return Vec::new() };
        match self.request(&c, json!({"op": "lives"}), LOOKUP).await {
            Ok(v) => serde_json::from_value(v).unwrap_or_default(),
            Err(e) => {
                tracing::warn!("cannot list the agent runner's agents: {e:#}");
                Vec::new()
            }
        }
    }

    /// Takes the live agent `info` back: its output after the runner's number `after` comes
    /// again, then whatever follows. Also says whether a turn can still end: one runs in the agent,
    /// or its end is among what comes again. Neither means the last message never reached the
    /// agent (the host died while sending it).
    pub async fn adopt(&self, info: LiveInfo, after: u64) -> Result<(LiveHandle, mpsc::Receiver<Out>, bool)> {
        let c = self.conn().ok_or_else(|| anyhow!("the agent runner is not connected"))?;
        let (out_tx, out) = mpsc::channel(1024);
        c.lives.lock().unwrap().insert(info.live.clone(), Sink { out: out_tx, last: after, marked: after });
        let v = match self.request(&c, json!({"op": "adopt", "live": info.live, "after": after}), LOOKUP).await {
            Ok(v) => v,
            Err(e) => {
                c.lives.lock().unwrap().remove(&info.live);
                return Err(e);
            }
        };
        let turn_can_end = v["busy"] == true || v["ending"] == true;
        Ok((self.handle(c, info), out, turn_can_end))
    }

    /// Stops a live agent nobody adopts.
    pub async fn stop(&self, live: &str) {
        if let Some(c) = self.conn() {
            let _ = self.request(&c, json!({"op": "shutdown", "live": live}), Duration::from_secs(20)).await;
        }
    }

    /// Tells the runner to stop its agents and exit (the host stops cleanly).
    pub async fn close(&self) {
        if let Some(c) = self.conn() {
            let _ = self.request(&c, json!({"op": "exit"}), Duration::from_secs(20)).await;
        }
    }

    /// The handle a session drives: its commands become lines to the runner.
    fn handle(&self, conn: Arc<Conn>, info: LiveInfo) -> LiveHandle {
        let (tx, mut cmds) = mpsc::channel::<LiveCmd>(32);
        let (ack_tx, mut acks) = mpsc::unbounded_channel::<u64>();
        let link = self.clone();
        let live = info.live.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    cmd = cmds.recv() => {
                        let line = match cmd {
                            Some(LiveCmd::Prompt { text, images }) => {
                                json!({"t": "prompt", "text": text, "images": images_json(&images)})
                            }
                            Some(LiveCmd::Steer { id, text, images }) => {
                                json!({"t": "steer", "id": id, "text": text, "images": images_json(&images)})
                            }
                            Some(LiveCmd::Interrupt) => json!({"t": "interrupt"}),
                            Some(LiveCmd::Answer { qid, text, answers }) => {
                                json!({"t": "answer", "qid": qid, "text": text, "answers": answers})
                            }
                            Some(LiveCmd::Permit { rid, decision, message, mode }) => {
                                json!({"t": "permit", "rid": rid, "decision": decision, "message": message, "mode": mode})
                            }
                            Some(LiveCmd::SetMode { mode }) => json!({"t": "mode", "mode": mode}),
                            Some(LiveCmd::GenerateTitle { description }) => {
                                json!({"t": "title", "description": description})
                            }
                            Some(LiveCmd::Shutdown { done }) => {
                                conn.lives.lock().unwrap().remove(&live);
                                let _ = link.request(&conn, json!({"op": "shutdown", "live": live}), Duration::from_secs(20)).await;
                                let _ = done.send(());
                                return;
                            }
                            None => {
                                // The session let go of the agent without a shutdown: stop it.
                                conn.lives.lock().unwrap().remove(&live);
                                let _ = conn.tx.send(json!({"op": "shutdown", "live": live, "id": 0}).to_string());
                                return;
                            }
                        };
                        let _ = conn.tx.send(json!({"op": "cmd", "live": live, "cmd": line}).to_string());
                    }
                    Some(n) = acks.recv() => {
                        let _ = conn.tx.send(json!({"op": "ack", "live": live, "upto": n}).to_string());
                    }
                }
            }
        });
        LiveHandle {
            tx,
            pid: info.pid,
            start: info.start,
            interrupt_grace: Duration::from_millis(info.grace_ms),
            ack: Some(ack_tx),
        }
    }
}
/// Images as they cross the pipe to the runner (already checked by the host, see [`Image::parse`]).
fn images_json(images: &[Image]) -> Value {
    images.iter().map(|i| json!({"name": i.name, "mime": i.mime, "data": &*i.data, "bytes": i.bytes})).collect()
}

/// The images of a command from the host.
fn images_from(v: &Value) -> Result<Vec<Image>> {
    let Some(list) = v.as_array() else { return Ok(Vec::new()) };
    list.iter()
        .map(|i| {
            Ok(Image {
                name: i["name"].as_str().unwrap_or_default().to_string(),
                mime: i["mime"].as_str().context("an image without a type")?.to_string(),
                data: i["data"].as_str().context("an image without data")?.into(),
                bytes: i["bytes"].as_u64().unwrap_or_default() as usize,
            })
        })
        .collect()
}

/// Sends an [`Out::Mark`] for every agent whose output moved on since the last one.
/// `all`, regardless of the time).
async fn mark(conn: &Conn) {
    let due: Vec<(mpsc::Sender<Out>, u64)> = conn
        .lives
        .lock()
        .unwrap()
        .values_mut()
        .filter(|s| s.last > s.marked)
        .map(|s| {
            s.marked = s.last;
            (s.out.clone(), s.last)
        })
        .collect();
    for (tx, n) in due {
        let _ = tx.send(Out::Mark(n)).await;
    }
}

// ---------------------------------------------------------------------------------------------
// Runner side
// ---------------------------------------------------------------------------------------------

/// The runner's agents, kept across connections.
#[derive(Default)]
pub struct Runner {
    detector: Detector,
    lives: Mutex<HashMap<String, Arc<RLive>>>,
    conn: Mutex<Option<mpsc::UnboundedSender<String>>>,
}

struct RLive {
    tx: mpsc::Sender<LiveCmd>,
    pid: u32,
    start: u64,
    grace: Duration,
    buf: Mutex<Buf>,
}

#[derive(Default)]
struct Buf {
    next: u64,
    /// Output not yet acknowledged: (number, line, whether it ends the turn).
    unacked: VecDeque<(u64, String, bool)>,
    bytes: usize,
    /// The connected host has this agent: new output goes out as it comes.
    attached: bool,
    busy: bool,
    gone: bool,
}

#[derive(Debug, Deserialize)]
struct Req {
    #[serde(default)]
    id: Option<u64>,
    op: String,
    #[serde(default)]
    live: Option<String>,
    #[serde(default)]
    custom: Vec<CustomAgent>,
    #[serde(default)]
    refresh: bool,
    #[serde(default)]
    runtime: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    hardened: bool,
    #[serde(default)]
    resume: Option<String>,
    /// A conversation to fork (the host sends it apart from `resume`, see the launch request).
    #[serde(default)]
    fork_from: Option<String>,
    #[serde(default)]
    permission_mode: Option<String>,
    #[serde(default)]
    after: u64,
    #[serde(default)]
    upto: u64,
    #[serde(default)]
    cmd: Option<Value>,
    #[serde(default)]
    path: Option<String>,
}

/// What a connection's end means for the runner.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// The host went away; wait for the next one.
    Lost,
    /// The host told the runner to exit.
    Exit,
}

impl Runner {
    pub fn live_count(&self) -> usize {
        self.lives.lock().unwrap().len()
    }

    /// Serves one authenticated host connection until it ends.
    pub async fn serve<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(self: &Arc<Self>, wire: Wire<S>) -> Ended {
        // Keep the handshake's reader: what the other side sent right after it may already be in
        // its buffer (a host that came back asks for the live agents at once).
        let (read, mut write) = tokio::io::split(wire.io);
        let mut read = BufReader::new(read);
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            while let Some(mut line) = rx.recv().await {
                line.push('\n');
                if write.write_all(line.as_bytes()).await.is_err() || write.flush().await.is_err() {
                    break;
                }
            }
        });
        // The new host takes agents back one by one (adopt); until then their output waits.
        for l in self.lives.lock().unwrap().values() {
            l.buf.lock().unwrap().attached = false;
        }
        *self.conn.lock().unwrap() = Some(tx.clone());
        let ended = loop {
            match read_line(&mut read).await {
                Ok(Some(v)) => match serde_json::from_value::<Req>(v) {
                    Ok(req) if req.op == "exit" => {
                        self.stop_all().await;
                        reply(&tx, req.id, Ok(Value::Null));
                        break Ended::Exit;
                    }
                    // An agent's commands (a message, then a steer during its turn) and acks go in
                    // the order they came: run here, one after the other.
                    Ok(req) if matches!(req.op.as_str(), "cmd" | "ack") => {
                        let id = req.id;
                        let res = self.handle(req).await;
                        if id.is_some_and(|i| i > 0) {
                            reply(&tx, id, res);
                        }
                    }
                    Ok(req) => {
                        // Other requests run concurrently: detection can take seconds and must not
                        // hold up the agents' commands.
                        let me = self.clone();
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            let id = req.id;
                            let res = me.handle(req).await;
                            if id.is_some_and(|i| i > 0) {
                                reply(&tx, id, res);
                            }
                        });
                    }
                    Err(e) => tracing::warn!("unreadable request from the host: {e}"),
                },
                Ok(None) => break Ended::Lost,
                Err(e) => {
                    tracing::warn!("host connection: {e:#}");
                    break Ended::Lost;
                }
            }
        };
        *self.conn.lock().unwrap() = None;
        for l in self.lives.lock().unwrap().values() {
            l.buf.lock().unwrap().attached = false;
        }
        drop(tx);
        // Lets the last answers out before the connection closes.
        let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
        ended
    }

    async fn handle(self: &Arc<Self>, req: Req) -> Result<Value> {
        match req.op.as_str() {
            "runtimes" => Ok(self.detector.report(&req.custom, req.refresh).await),
            "prepare" => {
                let runtime = req.runtime.as_deref().unwrap_or_default();
                let resolved = self.detector.resolve(runtime, &req.custom)?;
                let cwd = check_cwd(req.cwd.as_deref().unwrap_or_default())?;
                Ok(json!({
                    "id": resolved.id, "name": resolved.name, "program": resolved.program.display().to_string(),
                    "cwd": cwd.display().to_string(),
                }))
            }
            "launch" => self.launch(req).await,
            // A file for a preview, read with the user's rights (see `chat::preview`).
            "read" => {
                let path = PathBuf::from(req.path.unwrap_or_default());
                let bytes = tokio::task::spawn_blocking(move || super::preview::read_file(&path))
                    .await
                    .map_err(|e| anyhow!("file worker failed: {e}"))??;
                Ok(json!({"data": BASE64.encode(bytes)}))
            }
            "lives" => {
                let lives: Vec<LiveInfo> = self.lives.lock().unwrap().iter().map(|(k, l)| l.info(k)).collect();
                Ok(serde_json::to_value(lives)?)
            }
            "adopt" => {
                let live = req.live.unwrap_or_default();
                let l = self.get(&live)?;
                let conn = self.conn.lock().unwrap().clone();
                let mut b = l.buf.lock().unwrap();
                b.drop_upto(req.after);
                b.attached = true;
                if let Some(tx) = conn {
                    for (_, line, _) in &b.unacked {
                        let _ = tx.send(line.clone());
                    }
                }
                let ending = b.unacked.iter().any(|(_, _, e)| *e);
                Ok(json!({"busy": b.busy, "ending": ending}))
            }
            "ack" => {
                let live = req.live.unwrap_or_default();
                if let Ok(l) = self.get(&live) {
                    let mut b = l.buf.lock().unwrap();
                    b.drop_upto(req.upto);
                    if b.gone && b.unacked.is_empty() {
                        drop(b);
                        self.lives.lock().unwrap().remove(&live);
                    }
                }
                Ok(Value::Null)
            }
            "cmd" => {
                let live = req.live.unwrap_or_default();
                let l = self.get(&live)?;
                let c = req.cmd.unwrap_or_default();
                let cmd = match c["t"].as_str() {
                    Some("prompt") => {
                        l.buf.lock().unwrap().busy = true;
                        LiveCmd::Prompt {
                            text: c["text"].as_str().unwrap_or_default().to_string(),
                            images: images_from(&c["images"])?,
                        }
                    }
                    Some("steer") => LiveCmd::Steer {
                        id: c["id"].as_str().unwrap_or_default().to_string(),
                        text: c["text"].as_str().unwrap_or_default().to_string(),
                        images: images_from(&c["images"])?,
                    },
                    Some("interrupt") => LiveCmd::Interrupt,
                    Some("answer") => LiveCmd::Answer {
                        qid: c["qid"].as_str().unwrap_or_default().to_string(),
                        text: c["text"].as_str().unwrap_or_default().to_string(),
                        answers: serde_json::from_value(c["answers"].clone()).ok().flatten(),
                    },
                    Some("permit") => LiveCmd::Permit {
                        rid: c["rid"].as_str().unwrap_or_default().to_string(),
                        decision: serde_json::from_value(c["decision"].clone())?,
                        message: c["message"].as_str().map(str::to_string),
                        mode: c["mode"].as_str().map(str::to_string),
                    },
                    Some("mode") => LiveCmd::SetMode { mode: c["mode"].as_str().unwrap_or_default().to_string() },
                    Some("title") => LiveCmd::GenerateTitle {
                        description: c["description"].as_str().unwrap_or_default().to_string(),
                    },
                    _ => bail!("unknown command"),
                };
                let _ = l.tx.send(cmd).await;
                Ok(Value::Null)
            }
            "shutdown" => {
                let live = req.live.unwrap_or_default();
                let l = self.lives.lock().unwrap().remove(&live);
                if let Some(l) = l {
                    stop(&l).await;
                }
                Ok(Value::Null)
            }
            other => bail!("unknown request '{other}'"),
        }
    }

    fn get(&self, live: &str) -> Result<Arc<RLive>> {
        self.lives.lock().unwrap().get(live).cloned().ok_or_else(|| anyhow!("no agent '{live}' in the runner"))
    }

    async fn launch(self: &Arc<Self>, req: Req) -> Result<Value> {
        let live = req.live.clone().filter(|l| super::valid_token(l)).ok_or_else(|| anyhow!("bad agent id"))?;
        if self.lives.lock().unwrap().contains_key(&live) {
            bail!("agent '{live}' already runs");
        }
        let resolved = self.detector.resolve(req.runtime.as_deref().unwrap_or_default(), &req.custom)?;
        let cwd = check_cwd(req.cwd.as_deref().unwrap_or_default())?;
        let (handle, mut out) = adapters::launch(Launch {
            resolved: &resolved,
            cwd: &cwd,
            model: req.model.as_deref(),
            hardened: req.hardened,
            resume: req.resume.as_deref().or(req.fork_from.as_deref()),
            fork: req.resume.is_none() && req.fork_from.is_some(),
            permission_mode: req.permission_mode.as_deref(),
        })?;
        let l = Arc::new(RLive {
            tx: handle.tx,
            pid: handle.pid,
            start: handle.start,
            grace: handle.interrupt_grace,
            buf: Mutex::new(Buf { attached: true, ..Default::default() }),
        });
        self.lives.lock().unwrap().insert(live.clone(), l.clone());
        let info = l.info(&live);
        let me = Arc::downgrade(self);
        let id = live.clone();
        tokio::spawn(async move {
            loop {
                // Stop reading while the host is far behind; the agent waits on its pipe.
                while l.buf.lock().unwrap().bytes > MAX_UNACKED {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                let Some(o) = out.recv().await else { break };
                let Some(me) = me.upgrade() else { break };
                let ends = matches!(o, Out::Exited(_));
                let turn_over = matches!(o, Out::TurnEnd { .. } | Out::Exited(_));
                {
                    let mut b = l.buf.lock().unwrap();
                    if turn_over {
                        b.busy = false;
                    }
                    b.next += 1;
                    let line = json!({"op": "out", "live": id, "n": b.next, "out": o}).to_string();
                    b.bytes += line.len();
                    if b.attached
                        && let Some(tx) = me.conn.lock().unwrap().as_ref()
                    {
                        let _ = tx.send(line.clone());
                    }
                    let n = b.next;
                    b.unacked.push_back((n, line, turn_over));
                    if ends {
                        b.gone = true;
                    }
                }
                if ends {
                    break;
                }
            }
        });
        Ok(serde_json::to_value(info)?)
    }

    /// Stops every agent.
    pub async fn stop_all(&self) {
        let all: Vec<Arc<RLive>> = self.lives.lock().unwrap().drain().map(|(_, l)| l).collect();
        futures::future::join_all(all.iter().map(|l| stop(l))).await;
    }
}

impl RLive {
    fn info(&self, live: &str) -> LiveInfo {
        let b = self.buf.lock().unwrap();
        LiveInfo {
            live: live.to_string(),
            pid: self.pid,
            start: self.start,
            grace_ms: self.grace.as_millis() as u64,
            busy: b.busy,
            gone: b.gone,
        }
    }
}

impl Buf {
    fn drop_upto(&mut self, n: u64) {
        while self.unacked.front().is_some_and(|(k, _, _)| *k <= n) {
            let (_, line, _) = self.unacked.pop_front().expect("present");
            self.bytes -= line.len();
        }
    }
}

async fn stop(l: &RLive) {
    let (done, wait) = oneshot::channel();
    if l.tx.send(LiveCmd::Shutdown { done }).await.is_ok() {
        let _ = tokio::time::timeout(Duration::from_secs(15), wait).await;
    }
}

fn reply(tx: &mpsc::UnboundedSender<String>, id: Option<u64>, res: Result<Value>) {
    let Some(id) = id else { return };
    let line = match res {
        Ok(v) => json!({"id": id, "ok": v}),
        Err(e) => json!({"id": id, "err": format!("{e:#}")}),
    };
    let _ = tx.send(line.to_string());
}

/// The working folder as the user sees it: `~` expanded to the user's home, absolute, existing.
fn check_cwd(cwd: &str) -> Result<PathBuf> {
    let p = PathBuf::from(detect::expand_home(cwd.trim()));
    if !p.is_absolute() {
        bail!("the working folder must be an absolute path, not '{cwd}'");
    }
    if !p.is_dir() {
        bail!("the working folder {} does not exist on this host", p.display());
    }
    Ok(dunce::simplified(&p).to_path_buf())
}

/// The runner process: connects to the host's endpoint at `endpoint` (again after every break),
/// proves `key`, serves, and exits after `exit` or a long time without a host.
pub async fn run(endpoint: PathBuf, key: [u8; 32], idle_empty: Duration, idle_busy: Duration) -> Result<()> {
    let runner = Arc::new(Runner::default());
    let mut last_host = Instant::now();
    let mut refused = 0u32;
    loop {
        let idle = if runner.live_count() == 0 { idle_empty } else { idle_busy };
        if last_host.elapsed() >= idle {
            tracing::info!(agents = runner.live_count(), "no host for {} s; the agent runner stops", idle.as_secs());
            runner.stop_all().await;
            return Ok(());
        }
        let stream = match ar_net::ipc::Conn::connect(&endpoint).await {
            Ok(c) => c,
            Err(_) => {
                tokio::time::sleep(RETRY).await;
                continue;
            }
        };
        let (raw, _) = stream.into_raw();
        let mut wire = Wire::new(raw);
        if let Err(e) = handshake_runner(&mut wire, &key).await {
            refused += 1;
            if refused <= 3 {
                tracing::warn!("{e:#}");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        refused = 0;
        tracing::info!(endpoint = %endpoint.display(), agents = runner.live_count(), "connected to the host");
        let ended = runner.serve(wire).await;
        last_host = Instant::now();
        if ended == Ended::Exit {
            tracing::info!("the host told the agent runner to exit");
            return Ok(());
        }
        tracing::info!("the host went away; waiting for it to come back");
    }
}

#[cfg(test)]
mod tests;
