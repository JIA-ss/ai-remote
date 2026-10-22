//! Coding agents on this host (Claude Code, Codex, ACP agents) that a paired device starts and
//! chats with: the agent runs here, as the signed-in user, without asking before each action
//! (bypass mode), and the conversation streams to the controller beside the live view.
//!
//! - **Starting** is a tool call ([`crate::tools::chat`]): level admin, asks the owner by default
//!   (once per session), audited, refused while the owner has switched agents off here
//!   (`ai-remote chat disable`, which holds for bypassed devices too). It returns a ticket for the
//!   chat channel ([`crate::chat_channel`]), where messages go down and events come up.
//! - **A session** ([`Chat`]) owns one agent process at a time. The process stays up between turns
//!   (both Claude Code and Codex take further messages on the same process); after it ends — idle
//!   too long, the host restarted, it crashed, an interrupt that did not land — the session is
//!   *parked* and the next message starts a new process that resumes the agent's own conversation.
//! - **Events** ([`event`]) are numbered by the session and written to `<chat_dir>/<id>/events.jsonl`
//!   before they go out, so a viewer that comes back asks for what it missed. Messages typed while
//!   a turn runs wait in a queue and go out in order, unless they are sent to *steer*: then they
//!   join the running turn where the agent can take them (its caps say `steer`), and wait in the
//!   queue like any other otherwise.
//! - **What limits it**: at most [`MAX_LIVE`] agent processes at a time; the device that started a
//!   session must keep level admin (checked every few seconds, see [`ChatManager::enforce`]); a
//!   pause stops every running turn and holds new ones; the audit log records each session and
//!   each turn (who, a hash of the input, how it ended, how many tools of each kind ran), never the
//!   conversation itself, which stays in the session's directory on this host.
//!
//! The agents run with the user's rights and without asking: anything the user may do on this
//! computer, they may do, including changing this host's own settings. That is the point of the
//! feature and the reason starting one needs level admin and, by default, the owner's approval.

pub mod adapters;
pub mod detect;
pub mod event;
mod jsonrpc;
pub mod preview;
pub mod proc;
pub mod runtimes;

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use ar_net::Peer;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, mpsc};

use self::{
    adapters::{Launch, LiveCmd, LiveHandle},
    event::{By, Ev, Out, Queued, ToolContent, TurnStatus},
    runtimes::{ChatConfig, Protocol},
};

/// Agent processes that may run at the same time.
pub const MAX_LIVE: usize = 4;
/// Sessions kept on disk; older ended ones are deleted.
const MAX_KEPT: usize = 50;
/// Ended sessions older than this are deleted.
const KEEP_FOR: Duration = Duration::from_secs(30 * 24 * 3600);
/// A transcript longer than this ends its session.
const MAX_TRANSCRIPT: u64 = 50 * 1024 * 1024;
/// Messages that may wait for their turn.
const MAX_QUEUE: usize = 8;
/// Longest message accepted.
pub const MAX_INPUT: usize = 100_000;
/// An agent process with no turn running for this long is stopped (the session is parked).
const IDLE_PROCESS: Duration = Duration::from_secs(20 * 60);
/// A turn with no event for this long is interrupted.
const QUIET_TURN: Duration = Duration::from_secs(30 * 60);
/// How long a chat ticket can be redeemed after it was issued.
pub const TICKET_TTL: Duration = Duration::from_secs(120);

/// Where a session stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// The agent process is starting.
    Starting,
    /// Ready for a message.
    Idle,
    /// A turn runs.
    Running,
    /// A turn is being stopped.
    Interrupting,
    /// No process; the next message starts one that resumes the conversation.
    Parked,
    /// Over; only the transcript is left.
    Ended,
}

/// What `<id>/meta.json` holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub id: String,
    /// The agent's id (built in or custom).
    pub runtime: String,
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    pub cwd: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub hardened: bool,
    pub by: By,
    pub started_at: u64,
    pub state: State,
    /// The agent's own conversation id (Claude session, Codex thread, ACP session).
    #[serde(default)]
    pub proto_session: Option<String>,
    #[serde(default)]
    pub turns: u32,
    /// The running agent process, to stop it after a host restart.
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub pid_start: Option<u64>,
    #[serde(default)]
    pub ended_at: Option<u64>,
    #[serde(default)]
    pub end_reason: Option<String>,
    pub last_activity: u64,
}

/// A message on its way to the agent.
#[derive(Debug, Clone)]
pub struct Input {
    pub id: String,
    pub text: String,
    pub by: By,
}

/// A command for a session's driver.
#[derive(Debug)]
pub enum Cmd {
    Input(Input),
    /// Add this message to the running turn; with no turn running, or an agent that cannot take
    /// one, it is queued like [`Cmd::Input`] (with a `steer_queued` notice saying why).
    Steer(Input),
    /// Stop the current turn, then (with `then`) send this message first.
    Interrupt {
        then: Option<Input>,
    },
    Answer {
        qid: String,
        text: String,
    },
    Dequeue {
        id: String,
    },
    /// End the session; its process is stopped.
    End {
        reason: String,
    },
    /// Stop the process but keep the session (a pause, an idle process).
    Park {
        reason: String,
    },
}

/// Whether a ticket lets its holder send messages or only watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Rw,
    Ro,
}

#[derive(Debug, Clone)]
pub struct Ticket {
    pub chat: String,
    pub principal: Option<String>,
    pub role: Role,
    issued: Instant,
}

/// Every chat session of this host.
#[derive(Clone)]
pub struct ChatManager(Arc<Inner>);

struct Inner {
    dir: PathBuf,
    config_path: PathBuf,
    config: Mutex<ChatConfig>,
    /// Why `chat.toml` could not be read, if it could not (agents are off then).
    config_error: Option<String>,
    detector: detect::Detector,
    chats: Mutex<BTreeMap<String, Arc<Chat>>>,
    tickets: Mutex<HashMap<String, Ticket>>,
    audit: mpsc::UnboundedSender<Value>,
    /// Paused by the person at a controller: no turn may run.
    paused: Mutex<Option<String>>,
}

/// One chat session.
pub struct Chat {
    pub id: String,
    meta: Mutex<Meta>,
    log: Mutex<Log>,
    live: broadcast::Sender<Arc<str>>,
    cmd: mpsc::UnboundedSender<Cmd>,
    /// The device that started it, whose level is checked while it runs.
    pub creator: Peer,
    dir: PathBuf,
}

struct Log {
    file: Option<File>,
    seq: u64,
    turn: u32,
    bytes: u64,
}

impl ChatManager {
    /// Opens the sessions under `dir`, stopping agent processes a previous host left running.
    /// Returns the manager and the audit lines it produces (to be written to the audit log).
    pub fn new(dir: PathBuf, config_path: PathBuf) -> (Self, mpsc::UnboundedReceiver<Value>) {
        // A file that does not parse may have said `enabled = false`: switched off until fixed.
        let (config, config_error) = match ChatConfig::load(&config_path) {
            Ok(c) => (c, None),
            Err(e) => {
                tracing::warn!("{e:#}; coding agents are switched off until the file is fixed");
                (ChatConfig { enabled: false, agents: Vec::new() }, Some(format!("{e:#}")))
            }
        };
        let (audit, audit_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            dir,
            config_path,
            config: Mutex::new(config),
            config_error,
            detector: detect::Detector::default(),
            chats: Mutex::new(BTreeMap::new()),
            tickets: Mutex::new(HashMap::new()),
            audit,
            paused: Mutex::new(None),
        });
        let manager = Self(inner);
        manager.load();
        (manager, audit_rx)
    }

    pub fn enabled(&self) -> bool {
        self.0.config.lock().unwrap().enabled
    }

    /// Switches agents on or off here. Off ends every session.
    pub fn set_enabled(&self, on: bool) -> Result<()> {
        if let Some(e) = &self.0.config_error {
            bail!(
                "{} cannot be read, so it is not overwritten: {e}. Fix or remove it, then restart the host.",
                self.0.config_path.display()
            );
        }
        let mut cfg = self.0.config.lock().unwrap().clone();
        cfg.enabled = on;
        cfg.save(&self.0.config_path)?;
        *self.0.config.lock().unwrap() = cfg;
        if !on {
            for chat in self.all() {
                chat.send(Cmd::End { reason: "coding agents were switched off on this host".into() });
            }
        }
        self.audit(json!({"kind": "chat_admin", "action": if on { "enable" } else { "disable" }}));
        Ok(())
    }

    pub fn custom(&self) -> Vec<runtimes::CustomAgent> {
        self.0.config.lock().unwrap().agents.clone()
    }

    /// The installed agents.
    pub async fn runtimes(&self, refresh: bool) -> Value {
        let custom = self.custom();
        let mut v = self.0.detector.report(&custom, refresh).await;
        v["host"]["enabled"] = json!(self.enabled());
        v["host"]["max_live"] = json!(MAX_LIVE);
        v["host"]["config_error"] = json!(self.0.config_error);
        v
    }

    /// Every session, newest first.
    pub fn list(&self) -> Vec<Value> {
        let mut v: Vec<(u64, Value)> = self.all().iter().map(|c| (c.meta().started_at, c.describe())).collect();
        v.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        v.into_iter().map(|(_, v)| v).collect()
    }

    pub fn get(&self, id: &str) -> Result<Arc<Chat>> {
        self.0
            .chats
            .lock()
            .unwrap()
            .get(id.trim())
            .cloned()
            .ok_or_else(|| anyhow!("no chat session '{id}' on this host"))
    }

    fn all(&self) -> Vec<Arc<Chat>> {
        self.0.chats.lock().unwrap().values().cloned().collect()
    }

    /// Starts a session for `creator`: checks what can be checked now (switched on, agent
    /// installed, folder exists, room for another process) and returns it; the agent process
    /// starts with its first message.
    pub fn start(
        &self,
        creator: Peer,
        runtime: &str,
        cwd: &str,
        model: Option<String>,
        hardened: bool,
        prompt: Option<String>,
    ) -> Result<Arc<Chat>> {
        if let Some(e) = &self.0.config_error {
            bail!("coding agents are switched off on this host until its chat.toml is fixed: {e}");
        }
        if !self.enabled() {
            bail!("coding agents are switched off on this host (someone at it can run: ai-remote chat enable)");
        }
        let resolved = self.0.detector.resolve(runtime, &self.custom())?;
        let cwd_path = PathBuf::from(detect::expand_home(cwd.trim()));
        if !cwd_path.is_absolute() {
            bail!("the working folder must be an absolute path, not '{cwd}'");
        }
        if !cwd_path.is_dir() {
            bail!("the working folder {} does not exist on this host", cwd_path.display());
        }
        if let Some(m) = &model
            && !valid_token(m)
        {
            bail!("invalid model name '{m}'");
        }
        if self.live_count() >= MAX_LIVE {
            bail!("{MAX_LIVE} agents are already running here; end one first");
        }
        let id = format!("c{}", &random_hex()[..12]);
        let dir = self.0.dir.join(&id);
        std::fs::create_dir_all(&dir)?;
        let by = By { device: creator.name.clone(), principal: creator.principal.clone() };
        let meta = Meta {
            id: id.clone(),
            runtime: resolved.id.clone(),
            name: resolved.name.clone(),
            version: None,
            cwd: dunce::simplified(&cwd_path).display().to_string(),
            model,
            hardened,
            by: by.clone(),
            started_at: now_ms(),
            state: State::Parked,
            proto_session: None,
            turns: 0,
            pid: None,
            pid_start: None,
            ended_at: None,
            end_reason: None,
            last_activity: now_ms(),
        };
        let chat = self.insert(meta, creator, &dir)?;
        self.audit(json!({
            "kind": "chat_session", "action": "start", "chat": id, "runtime": resolved.id,
            "program": resolved.program.display().to_string(), "cwd": chat.meta().cwd, "by": by.device,
            "principal": by.principal, "hardened": hardened,
        }));
        if let Some(text) = prompt.filter(|t| !t.trim().is_empty()) {
            chat.send(Cmd::Input(Input { id: format!("u{}", &random_hex()[..8]), text, by }));
        }
        self.prune();
        Ok(chat)
    }

    fn insert(&self, meta: Meta, creator: Peer, dir: &Path) -> Result<Arc<Chat>> {
        let (seq, turn, bytes) = scan_log(&dir.join("events.jsonl"));
        let file = (meta.state != State::Ended)
            .then(|| OpenOptions::new().create(true).append(true).open(dir.join("events.jsonl")))
            .transpose()?;
        if let Some(f) = &file {
            restrict(f, &dir.join("events.jsonl"));
        }
        let (live, _) = broadcast::channel(1024);
        let (tx, rx) = mpsc::unbounded_channel();
        let ended = meta.state == State::Ended;
        let chat = Arc::new(Chat {
            id: meta.id.clone(),
            meta: Mutex::new(meta),
            log: Mutex::new(Log { file, seq, turn, bytes }),
            live,
            cmd: tx,
            creator,
            dir: dir.to_path_buf(),
        });
        chat.save();
        self.0.chats.lock().unwrap().insert(chat.id.clone(), chat.clone());
        if !ended {
            tokio::spawn(drive(chat.clone(), Arc::downgrade(&self.0), rx));
        }
        Ok(chat)
    }

    /// Agent processes running now.
    pub fn live_count(&self) -> usize {
        self.all().iter().filter(|c| c.meta().pid.is_some()).count()
    }

    pub fn issue_ticket(&self, chat: &str, principal: Option<String>, role: Role) -> String {
        let id = random_hex();
        let mut t = self.0.tickets.lock().unwrap();
        t.retain(|_, t| t.issued.elapsed() < TICKET_TTL);
        t.insert(id.clone(), Ticket { chat: chat.to_string(), principal, role, issued: Instant::now() });
        id
    }

    /// Takes the ticket `id` for the device `principal`; it cannot be used again.
    pub fn redeem(&self, id: &str, principal: Option<&str>) -> Result<Ticket> {
        let mut t = self.0.tickets.lock().unwrap();
        let Some(ticket) = t.get(id.trim()) else { bail!("unknown or already used chat ticket; ask for a new one") };
        if ticket.principal.is_none() || ticket.principal.as_deref() != principal {
            bail!("this chat ticket was issued to another device");
        }
        let ticket = t.remove(id.trim()).expect("present");
        if ticket.issued.elapsed() >= TICKET_TTL {
            bail!("the chat ticket expired; ask for a new one");
        }
        Ok(ticket)
    }

    /// Pauses (`Some(by)`) or resumes (`None`) every agent: a pause interrupts running turns and
    /// holds new messages until resumed.
    pub fn set_paused(&self, by: Option<String>) {
        let was = std::mem::replace(&mut *self.0.paused.lock().unwrap(), by.clone());
        if was.is_none()
            && let Some(by) = by
        {
            for chat in self.all() {
                if matches!(chat.state(), State::Running | State::Starting) {
                    chat.send(Cmd::Interrupt { then: None });
                    chat.emit(Ev::Notice {
                        level: "warn",
                        code: "paused",
                        text: format!("{by} paused this computer; the turn was stopped"),
                    });
                }
            }
            self.audit(json!({"kind": "chat_admin", "action": "pause_stop"}));
        }
    }

    /// Ends the sessions of devices that lost level admin. `level_of` answers with a device's
    /// effective level now.
    pub fn enforce(&self, still_admin: impl Fn(&Peer) -> bool) {
        for chat in self.all() {
            if chat.state() != State::Ended && !still_admin(&chat.creator) {
                chat.send(Cmd::End { reason: "the device that started it no longer has level admin here".into() });
                self.audit(json!({"kind": "chat_admin", "action": "revoke_end", "chat": chat.id}));
            }
        }
    }

    fn audit(&self, v: Value) {
        let _ = self.0.audit.send(v);
    }

    /// Loads the sessions on disk. A session whose agent was running when the host stopped is
    /// parked, and the processes it left are stopped (on Windows the Job Object already did).
    fn load(&self) {
        let Ok(rd) = std::fs::read_dir(&self.0.dir) else { return };
        for entry in rd.flatten() {
            let dir = entry.path();
            let Ok(text) = std::fs::read_to_string(dir.join("meta.json")) else { continue };
            let Ok(mut meta) = serde_json::from_str::<Meta>(&text) else { continue };
            let was_running = matches!(meta.state, State::Running | State::Interrupting | State::Starting);
            if let (Some(pid), Some(start)) = (meta.pid.take(), meta.pid_start.take()) {
                let reaped = reap(pid, start);
                if reaped > 0 {
                    self.audit(
                        json!({"kind": "chat_admin", "action": "orphan_reaped", "chat": meta.id, "processes": reaped}),
                    );
                }
            }
            if meta.state != State::Ended {
                meta.state = State::Parked;
            }
            let creator = Peer {
                name: meta.by.device.clone(),
                principal: meta.by.principal.clone(),
                user_id: None,
                level: None,
                revoked: false,
            };
            match self.insert(meta, creator, &dir) {
                Ok(chat) if was_running => {
                    chat.emit(Ev::TurnEnd {
                        status: TurnStatus::Failed,
                        final_text: None,
                        duration_ms: 0,
                        reason: Some("the host restarted during this turn".into()),
                    });
                    chat.emit_state(&VecDeque::new());
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("cannot load chat session {}: {e:#}", dir.display()),
            }
        }
    }

    /// Deletes ended sessions beyond [`MAX_KEPT`] or older than [`KEEP_FOR`].
    fn prune(&self) {
        let mut ended: Vec<Arc<Chat>> = self.all().into_iter().filter(|c| c.state() == State::Ended).collect();
        ended.sort_by_key(|c| c.meta().started_at);
        let total = self.0.chats.lock().unwrap().len();
        let mut excess = total.saturating_sub(MAX_KEPT);
        let cutoff = now_ms().saturating_sub(KEEP_FOR.as_millis() as u64);
        for c in ended {
            let old = c.meta().ended_at.is_some_and(|t| t < cutoff);
            if excess == 0 && !old {
                continue;
            }
            excess = excess.saturating_sub(1);
            self.0.chats.lock().unwrap().remove(&c.id);
            let _ = std::fs::remove_dir_all(&c.dir);
        }
    }
}

impl Chat {
    pub fn meta(&self) -> Meta {
        self.meta.lock().unwrap().clone()
    }

    pub fn state(&self) -> State {
        self.meta.lock().unwrap().state
    }

    pub fn send(&self, cmd: Cmd) -> bool {
        self.cmd.send(cmd).is_ok()
    }

    /// What `chat_sessions` shows.
    pub fn describe(&self) -> Value {
        let m = self.meta();
        json!({
            "id": m.id, "runtime": m.runtime, "name": m.name, "version": m.version, "cwd": m.cwd,
            "model": m.model, "state": m.state, "by": m.by, "started_at": m.started_at, "turns": m.turns,
            "last_activity": m.last_activity, "ended_at": m.ended_at, "end_reason": m.end_reason,
            "seq": self.log.lock().unwrap().seq,
        })
    }

    fn save(&self) {
        let m = self.meta();
        let path = self.dir.join("meta.json");
        if let Err(e) = ar_core::store::write_json_atomic(&path, &m) {
            tracing::warn!("cannot save chat session {}: {e:#}", m.id);
        }
        ar_core::store::restrict_permissions(&path);
    }

    fn set_meta(&self, f: impl FnOnce(&mut Meta)) {
        f(&mut self.meta.lock().unwrap());
        self.save();
    }

    /// Numbers `ev`, writes it to the transcript and sends it to viewers.
    pub fn emit(&self, ev: Ev) {
        let mut log = self.log.lock().unwrap();
        if matches!(ev, Ev::TurnStart {}) {
            log.turn += 1;
        }
        log.seq += 1;
        // The envelope first, so `seq` can be read without parsing the line (see [`line_seq`]).
        let body = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
        let text = format!("{{\"seq\":{},\"ts\":{},\"turn\":{},{}", log.seq, now_ms(), log.turn, &body[1..]);
        if let Some(f) = log.file.as_mut() {
            let ok = f.write_all(text.as_bytes()).and_then(|_| f.write_all(b"\n")).and_then(|_| f.flush());
            if let Err(e) = ok {
                tracing::warn!("cannot write chat transcript {}: {e}", self.id);
            }
        }
        log.bytes += text.len() as u64 + 1;
        let _ = self.live.send(text.into());
    }

    fn emit_state(&self, queue: &VecDeque<Input>) {
        let state = self.state();
        let queued = queue.iter().map(|i| Queued { id: i.id.clone(), text: i.text.clone() }).collect();
        self.emit(Ev::State { state, queued });
    }

    fn set_state(&self, state: State, queue: &VecDeque<Input>) {
        if self.state() != state {
            self.set_meta(|m| m.state = state);
            self.emit_state(queue);
        }
    }

    fn transcript_full(&self) -> bool {
        self.log.lock().unwrap().bytes > MAX_TRANSCRIPT
    }

    /// The events after `since` from the transcript, then every new one: returns the lines on
    /// disk and a receiver for later lines (each later line has a `seq` above the last returned).
    pub fn follow(&self, since: u64) -> (Vec<String>, u64, broadcast::Receiver<Arc<str>>) {
        let (rx, upto) = {
            let log = self.log.lock().unwrap();
            (self.live.subscribe(), log.seq)
        };
        let mut lines = Vec::new();
        if let Ok(f) = File::open(self.dir.join("events.jsonl")) {
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                match line_seq(&line) {
                    Some(s) if s > since && s <= upto => lines.push(line),
                    _ => {}
                }
            }
        }
        (lines, upto, rx)
    }
}

/// The `seq` of a transcript line.
pub fn line_seq(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("{\"seq\":")?;
    rest[..rest.find(',')?].parse().ok()
}

/// Reads an existing transcript: last `seq`, last turn and size.
fn scan_log(path: &Path) -> (u64, u32, u64) {
    let Ok(f) = File::open(path) else { return (0, 0, 0) };
    let bytes = f.metadata().map(|m| m.len()).unwrap_or(0);
    let (mut seq, mut turn) = (0, 0);
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        if let Some(s) = line_seq(&line) {
            seq = s;
        }
        if let Some(t) = serde_json::from_str::<Value>(&line).ok().and_then(|v| v["turn"].as_u64()) {
            turn = t as u32;
        }
    }
    (seq, turn, bytes)
}

fn restrict(_f: &File, path: &Path) {
    ar_core::store::restrict_permissions(path);
}

/// Stops what a previous host's agent process `pid` (started at `start`) left running. Returns how
/// many processes were stopped.
fn reap(pid: u32, start: u64) -> usize {
    use crate::tools::jobs::{ProcTable, Root};
    let table = ProcTable::read();
    let root = Root { pid, start, alive_until: Some(start + 365 * 24 * 3600) };
    table.members(&root).iter().filter(|p| table.kill(p)).count()
}

/// The commands of a running session, applied by one task per session so its state changes in
/// order: messages, the agent's events, interrupts and timers.
async fn drive(chat: Arc<Chat>, manager: Weak<Inner>, mut rx: mpsc::UnboundedReceiver<Cmd>) {
    let mut d = Driver {
        chat,
        manager,
        queue: VecDeque::new(),
        live: None,
        out: None,
        turn: None,
        idle_since: Instant::now(),
        seen: VecDeque::new(),
        steer: false,
        steering: Vec::new(),
        delivered: HashSet::new(),
    };
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(cmd) => if d.command(cmd).await { break },
                None => break,
            },
            out = next_out(&mut d.out) => d.adapter(out).await,
            _ = tick.tick() => d.timers().await,
        }
    }
}

async fn next_out(out: &mut Option<mpsc::Receiver<Out>>) -> Option<Out> {
    match out {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

struct Turn {
    input: Input,
    started: Instant,
    last_event: Instant,
    interrupt_deadline: Option<Instant>,
    /// A message to send first once this turn has stopped.
    then: Option<Input>,
    tools: BTreeMap<&'static str, u32>,
    /// Messages that joined this turn.
    steers: u32,
}

struct Driver {
    chat: Arc<Chat>,
    manager: Weak<Inner>,
    queue: VecDeque<Input>,
    live: Option<LiveHandle>,
    out: Option<mpsc::Receiver<Out>>,
    turn: Option<Turn>,
    idle_since: Instant,
    /// Ids of recent messages: a controller that lost its connection may send one again, and it
    /// must not run twice.
    seen: VecDeque<String>,
    /// The running agent takes messages during a turn.
    steer: bool,
    /// Messages sent to steer the running turn that the agent has not answered yet.
    steering: Vec<Input>,
    /// Queued messages the agent already has: it runs them as a turn of its own (a steer that came
    /// too late for the turn it was meant for), so they are not sent again.
    delivered: HashSet<String>,
}

/// Message ids remembered for [`Driver::seen`].
const SEEN: usize = 128;

impl Driver {
    /// Whether the message `id` was taken before; remembers it otherwise.
    fn repeated(&mut self, id: &str) -> bool {
        if self.seen.iter().any(|s| s == id) {
            return true;
        }
        if self.seen.len() == SEEN {
            self.seen.pop_front();
        }
        self.seen.push_back(id.to_string());
        false
    }

    fn manager(&self) -> Option<ChatManager> {
        self.manager.upgrade().map(ChatManager)
    }

    /// Applies a command; returns whether the session is over.
    async fn command(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Input(input) => {
                if self.repeated(&input.id) {
                    return false;
                }
                self.enqueue(input).await;
            }
            Cmd::Steer(input) => {
                if self.repeated(&input.id) {
                    return false;
                }
                let running = self.turn.as_ref().is_some_and(|t| t.interrupt_deadline.is_none());
                if running
                    && self.steer
                    && let Some(l) = &self.live
                    && l.tx.send(LiveCmd::Steer { id: input.id.clone(), text: input.text.clone() }).await.is_ok()
                {
                    self.steering.push(input);
                    return false;
                }
                let text = if !running {
                    "no turn is running, so the message starts one".to_string()
                } else if !self.steer {
                    format!("{} cannot take a message during a turn; it waits for the next one", self.chat.meta().name)
                } else {
                    "the agent is not reachable now; the message waits for the next turn".to_string()
                };
                self.chat.emit(Ev::Notice { level: "info", code: "steer_queued", text });
                self.enqueue(input).await;
            }
            Cmd::Interrupt { then } => {
                let then = then.filter(|i| !self.repeated(&i.id));
                if let Some(t) = self.turn.as_mut() {
                    if t.interrupt_deadline.is_none() {
                        let grace = self.live.as_ref().map(|l| l.interrupt_grace).unwrap_or(Duration::from_secs(5));
                        t.interrupt_deadline = Some(Instant::now() + grace);
                        if let Some(l) = &self.live {
                            let _ = l.tx.send(LiveCmd::Interrupt).await;
                        }
                        self.chat.set_state(State::Interrupting, &self.queue);
                    }
                    if then.is_some() {
                        t.then = then;
                    }
                } else if let Some(input) = then {
                    self.queue.push_front(input);
                    self.next_turn().await;
                }
            }
            Cmd::Answer { qid, text } => match &self.live {
                Some(l) => {
                    let _ = l.tx.send(LiveCmd::Answer { qid, text }).await;
                }
                None => self.chat.emit(Ev::Notice {
                    level: "warn",
                    code: "no_question",
                    text: "nothing is waiting for an answer".into(),
                }),
            },
            Cmd::Dequeue { id } => {
                self.queue.retain(|i| i.id != id);
                self.chat.emit_state(&self.queue);
            }
            Cmd::Park { reason } => {
                if self.live.is_some() {
                    self.stop_process(&reason).await;
                }
            }
            Cmd::End { reason } => {
                self.stop_process(&reason).await;
                self.queue.clear();
                self.chat.set_meta(|m| {
                    m.state = State::Ended;
                    m.ended_at = Some(now_ms());
                    m.end_reason = Some(reason.clone());
                });
                self.chat.emit_state(&self.queue);
                self.chat.log.lock().unwrap().file = None;
                if let Some(m) = self.manager() {
                    m.audit(json!({"kind": "chat_session", "action": "end", "chat": self.chat.id, "reason": reason}));
                }
                return true;
            }
        }
        false
    }

    /// Queues a message and starts it if nothing runs.
    async fn enqueue(&mut self, input: Input) {
        if self.queue.len() >= MAX_QUEUE {
            self.chat.emit(Ev::Notice {
                level: "warn",
                code: "queue_full",
                text: format!("{MAX_QUEUE} messages are already waiting; this one was dropped"),
            });
            return;
        }
        self.queue.push_back(input);
        self.chat.emit_state(&self.queue);
        self.next_turn().await;
    }

    /// Puts `input` in the queue ahead of every message the agent does not have yet (behind the
    /// ones it has, which run first).
    fn queue_early(&mut self, input: Input) {
        let at = self.queue.iter().take_while(|i| self.delivered.contains(&i.id)).count();
        self.queue.insert(at, input);
    }

    /// The agent answered a steer.
    async fn steer_answer(&mut self, out: Out) {
        let id = match &out {
            Out::Steered { id } | Out::SteerLate { id } | Out::SteerRejected { id, .. } => id.clone(),
            _ => return,
        };
        let input = match self.steering.iter().position(|i| i.id == id) {
            Some(at) => self.steering.remove(at),
            None => match self.queue.iter().position(|i| i.id == id && self.delivered.contains(&i.id)) {
                // A message the agent had for a turn of its own joined the running one instead.
                Some(at) if matches!(out, Out::Steered { .. }) => {
                    self.delivered.remove(&id);
                    self.queue.remove(at).expect("present")
                }
                _ => return,
            },
        };
        match out {
            Out::Steered { .. } => {
                self.chat.emit(Ev::User { input_id: input.id, by: input.by, text: input.text, steer: true });
                if let Some(t) = self.turn.as_mut() {
                    t.steers += 1;
                }
                self.chat.set_meta(|m| m.last_activity = now_ms());
                self.chat.emit_state(&self.queue);
            }
            Out::SteerLate { .. } => {
                self.chat.emit(Ev::Notice {
                    level: "info",
                    code: "steer_queued",
                    text: "the agent finished this turn before it read the message; the message runs as the next turn"
                        .into(),
                });
                self.delivered.insert(input.id.clone());
                self.queue_early(input);
                self.chat.emit_state(&self.queue);
            }
            Out::SteerRejected { reason, .. } => {
                self.chat.emit(Ev::Notice {
                    level: "info",
                    code: "steer_queued",
                    text: format!(
                        "the agent did not take the message during the turn ({reason}); it waits for the next one"
                    ),
                });
                self.queue_early(input);
                self.chat.emit_state(&self.queue);
                self.next_turn().await;
            }
            _ => {}
        }
    }

    /// The agent process is gone: what it had not answered or run yet is sent again to the next one.
    fn requeue_unanswered(&mut self) {
        self.delivered.clear();
        for input in std::mem::take(&mut self.steering).into_iter().rev() {
            self.queue.push_front(input);
        }
    }

    /// Starts the next waiting message when nothing runs.
    async fn next_turn(&mut self) {
        if self.turn.is_some() || self.queue.is_empty() {
            return;
        }
        let Some(manager) = self.manager() else { return };
        if let Some(by) = manager.0.paused.lock().unwrap().clone() {
            self.chat.emit(Ev::Notice {
                level: "warn",
                code: "paused",
                text: format!("{by} paused this computer; messages wait until it resumes"),
            });
            return;
        }
        if self.chat.transcript_full() {
            self.chat.send(Cmd::End { reason: "the transcript reached its size limit".into() });
            return;
        }
        if self.live.is_none() {
            if manager.live_count() >= MAX_LIVE {
                self.chat.emit(Ev::Notice {
                    level: "warn",
                    code: "too_many",
                    text: format!("{MAX_LIVE} agents are already running here; the message waits until one stops"),
                });
                return;
            }
            if let Err(e) = self.launch(&manager) {
                self.chat.emit(Ev::Notice { level: "error", code: "start_failed", text: format!("{e:#}") });
                self.chat.set_state(State::Parked, &self.queue);
                // The message stays queued; it goes out with the next attempt.
                return;
            }
        }
        let input = self.queue.pop_front().expect("not empty");
        let Some(live) = &self.live else { return };
        // A message the agent already has starts its turn by itself.
        if !self.delivered.remove(&input.id)
            && live.tx.send(LiveCmd::Prompt { text: input.text.clone() }).await.is_err()
        {
            self.queue.push_front(input);
            return;
        }
        self.chat.emit(Ev::User {
            input_id: input.id.clone(),
            by: input.by.clone(),
            text: input.text.clone(),
            steer: false,
        });
        self.chat.emit(Ev::TurnStart {});
        self.chat.set_meta(|m| {
            m.turns += 1;
            m.last_activity = now_ms();
        });
        self.turn = Some(Turn {
            input,
            started: Instant::now(),
            last_event: Instant::now(),
            interrupt_deadline: None,
            then: None,
            tools: BTreeMap::new(),
            steers: 0,
        });
        self.chat.set_state(State::Running, &self.queue);
    }

    fn launch(&mut self, manager: &ChatManager) -> Result<()> {
        let meta = self.chat.meta();
        let resolved = manager.0.detector.resolve(&meta.runtime, &manager.custom())?;
        let (handle, out) = adapters::launch(Launch {
            resolved: &resolved,
            cwd: Path::new(&meta.cwd),
            model: meta.model.as_deref(),
            hardened: meta.hardened,
            resume: meta.proto_session.as_deref(),
        })?;
        self.chat.set_meta(|m| {
            m.pid = Some(handle.pid);
            m.pid_start = Some(handle.start);
        });
        if meta.proto_session.is_some() {
            self.chat.emit(Ev::Notice {
                level: "info",
                code: "resumed",
                text: format!("{} restarted and resumes the conversation", meta.name),
            });
        }
        self.steer = resolved.caps.steer;
        self.live = Some(handle);
        self.out = Some(out);
        self.chat.set_state(State::Starting, &self.queue);
        Ok(())
    }

    async fn adapter(&mut self, out: Option<Out>) {
        let Some(out) = out else {
            self.process_gone("the agent stopped".into()).await;
            return;
        };
        let mut next = Some(out);
        while let Some(out) = next.take() {
            next = self.handle(out).await;
        }
    }

    /// Applies one adapter message; returns a message read ahead while joining text that still
    /// has to be applied.
    async fn handle(&mut self, out: Out) -> Option<Out> {
        if let Some(t) = self.turn.as_mut() {
            t.last_event = Instant::now();
        }
        match out {
            Out::Ev(ev) => {
                if let (Ev::ToolCall { kind, .. }, Some(t)) = (&ev, self.turn.as_mut()) {
                    let k = serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(kind_name)).unwrap_or("other");
                    *t.tools.entry(k).or_default() += 1;
                }
                let held = if matches!(ev, Ev::Message { delta: true, .. } | Ev::Thought { delta: true, .. })
                    || output_chunk(&ev).is_some()
                {
                    let (merged, held) = self.coalesce(ev);
                    self.chat.emit(merged);
                    held
                } else {
                    self.chat.emit(ev);
                    None
                };
                if self.chat.state() == State::Starting && self.turn.is_some() {
                    self.chat.set_state(State::Running, &self.queue);
                }
                return held;
            }
            Out::Session(id) => {
                self.chat.set_meta(|m| m.proto_session = Some(id));
                if self.chat.state() == State::Starting {
                    let s = if self.turn.is_some() { State::Running } else { State::Idle };
                    self.chat.set_state(s, &self.queue);
                }
            }
            out @ (Out::Steered { .. } | Out::SteerLate { .. } | Out::SteerRejected { .. }) => {
                self.steer_answer(out).await
            }
            Out::TurnEnd { status, final_text, reason } => self.end_turn(status, final_text, reason).await,
            Out::Exited(why) => self.process_gone(why).await,
        }
        None
    }

    /// Joins deltas of the same message that are already waiting, so a fast agent does not turn
    /// into one event per token. Returns the joined event and the first message that did not fit.
    fn coalesce(&mut self, first: Ev) -> (Ev, Option<Out>) {
        if let Some((call, _)) = output_chunk(&first) {
            let call = call.to_string();
            let Ev::ToolUpdate { content, .. } = first else { unreachable!() };
            let Some(ToolContent::Text { text: mut joined }) = content.into_iter().next() else { unreachable!() };
            let mut held = None;
            if let Some(rx) = self.out.as_mut() {
                while joined.len() < 16 * 1024 {
                    match rx.try_recv() {
                        Ok(Out::Ev(ev)) if output_chunk(&ev).is_some_and(|(c, _)| c == call) => {
                            joined.push_str(output_chunk(&ev).expect("a chunk").1)
                        }
                        Ok(other) => {
                            held = Some(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }
            }
            let ev = Ev::ToolUpdate {
                call_id: call,
                status: None,
                content: vec![ToolContent::Text { text: joined }],
                truncated: false,
            };
            return (ev, held);
        }
        let (is_thought, id, mut text) = match first {
            Ev::Message { id, text, .. } => (false, id, text),
            Ev::Thought { id, text, .. } => (true, id, text),
            other => return (other, None),
        };
        let mut held = None;
        if let Some(rx) = self.out.as_mut() {
            while text.len() < 16 * 1024 {
                match rx.try_recv() {
                    Ok(Out::Ev(Ev::Message { id: i, text: t, delta: true })) if !is_thought && i == id => {
                        text.push_str(&t)
                    }
                    Ok(Out::Ev(Ev::Thought { id: i, text: t, delta: true })) if is_thought && i == id => {
                        text.push_str(&t)
                    }
                    Ok(other) => {
                        held = Some(other);
                        break;
                    }
                    Err(_) => break,
                }
            }
        }
        let ev = if is_thought { Ev::Thought { id, text, delta: true } } else { Ev::Message { id, text, delta: true } };
        (ev, held)
    }

    async fn end_turn(&mut self, status: TurnStatus, final_text: Option<String>, reason: Option<String>) {
        let Some(turn) = self.turn.take() else { return };
        let status = if turn.interrupt_deadline.is_some() && status == TurnStatus::Failed {
            TurnStatus::Interrupted
        } else {
            status
        };
        let duration_ms = turn.started.elapsed().as_millis() as u64;
        self.chat.emit(Ev::TurnEnd { status, final_text, duration_ms, reason: reason.clone() });
        self.chat.set_meta(|m| m.last_activity = now_ms());
        if let Some(m) = self.manager() {
            let hash = data_encoding::HEXLOWER.encode(&Sha256::digest(turn.input.text.as_bytes()));
            m.audit(json!({
                "kind": "chat_turn", "chat": self.chat.id, "runtime": self.chat.meta().runtime,
                "turn": self.chat.log.lock().unwrap().turn, "by": turn.input.by.device, "principal": turn.input.by.principal,
                "input_sha256": hash, "input_chars": turn.input.text.chars().count(), "outcome": status,
                "duration_ms": duration_ms, "tools": turn.tools, "steers": turn.steers, "reason": reason,
            }));
        }
        self.idle_since = Instant::now();
        let state = if self.live.is_some() { State::Idle } else { State::Parked };
        self.chat.set_state(state, &self.queue);
        if let Some(then) = turn.then {
            self.queue.push_front(then);
        }
        self.next_turn().await;
    }

    /// The agent process is gone: the turn (if any) failed, the session is parked.
    async fn process_gone(&mut self, why: String) {
        self.live = None;
        self.out = None;
        self.requeue_unanswered();
        self.chat.set_meta(|m| {
            m.pid = None;
            m.pid_start = None;
        });
        if self.turn.is_some() {
            let interrupted = self.turn.as_ref().is_some_and(|t| t.interrupt_deadline.is_some());
            let status = if interrupted { TurnStatus::Interrupted } else { TurnStatus::Failed };
            self.end_turn(status, None, Some(why)).await;
        } else {
            self.chat.emit(Ev::Notice { level: "info", code: "stopped", text: why });
            self.chat.set_state(State::Parked, &self.queue);
        }
    }

    /// Stops the agent process (closing its input first) and parks the session.
    async fn stop_process(&mut self, reason: &str) {
        let Some(live) = self.live.take() else { return };
        self.out = None;
        self.requeue_unanswered();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        if live.tx.send(LiveCmd::Shutdown { done: done_tx }).await.is_ok() {
            let _ = tokio::time::timeout(Duration::from_secs(15), done_rx).await;
        }
        drop(live);
        self.chat.set_meta(|m| {
            m.pid = None;
            m.pid_start = None;
        });
        if self.turn.is_some() {
            self.end_turn(TurnStatus::Interrupted, None, Some(reason.to_string())).await;
        }
        if self.chat.state() != State::Ended {
            self.chat.set_state(State::Parked, &self.queue);
        }
    }

    async fn timers(&mut self) {
        let now = Instant::now();
        if let Some(t) = &self.turn {
            if t.interrupt_deadline.is_some_and(|d| now >= d) {
                self.chat.emit(Ev::Notice {
                    level: "warn",
                    code: "killed",
                    text: "the agent did not stop in time; its process was ended".into(),
                });
                self.stop_process("stopped by force after an interrupt").await;
                return;
            }
            if t.interrupt_deadline.is_none() && t.last_event.elapsed() >= QUIET_TURN {
                self.chat.emit(Ev::Notice {
                    level: "warn",
                    code: "quiet",
                    text: format!("no event for {} minutes; the turn is stopped", QUIET_TURN.as_secs() / 60),
                });
                let _ = self.command(Cmd::Interrupt { then: None }).await;
            }
        } else if self.live.is_some() && self.idle_since.elapsed() >= IDLE_PROCESS {
            self.stop_process("idle").await;
        }
    }
}

/// A piece of a running tool's output (a tool update with only text): its call and the text.
fn output_chunk(ev: &Ev) -> Option<(&str, &str)> {
    match ev {
        Ev::ToolUpdate { call_id, status: None, content, truncated: false } => match content.as_slice() {
            [ToolContent::Text { text }] => Some((call_id, text)),
            _ => None,
        },
        _ => None,
    }
}

fn kind_name(s: &str) -> &'static str {
    match s {
        "read" => "read",
        "edit" | "delete" | "move" => "edit",
        "search" => "search",
        "execute" => "execute",
        "fetch" => "fetch",
        _ => "other",
    }
}

/// A model name or resume id: no spaces or shell metacharacters (they end up in a command line).
pub fn valid_token(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:/@[]".contains(c))
}

pub fn random_hex() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    data_encoding::HEXLOWER.encode(&b)
}

pub fn now_ms() -> u64 {
    crate::audit::now_ms() as u64
}

/// Which protocol a session speaks, for timeouts.
pub fn interrupt_grace(p: Protocol) -> Duration {
    match p {
        Protocol::ClaudeStreamJson => Duration::from_secs(10),
        Protocol::CodexAppServer => Duration::from_secs(3),
        Protocol::Acp => Duration::from_secs(5),
    }
}

#[cfg(test)]
mod tests;
