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
//!
//! **Where the agents run** ([`Backend`]): a host that runs as the user runs them itself. A host
//! that runs as SYSTEM (Windows service mode) never executes anything of the user's: a runner it
//! starts in the user's session with the user's token finds and runs them ([`runner`]), and the
//! sessions here drive them over a local pipe. That runner outlives a host restart (Windows 11
//! ends the host when the screen locks); the restarted host takes the running agents back
//! ([`ChatManager::adopt_all`]) and their sessions go on.

pub mod adapters;
pub mod detect;
pub mod event;
pub mod files;
pub mod git;
mod jsonrpc;
pub mod preview;
pub mod proc;
pub mod runner;
#[cfg(windows)]
pub mod runner_win;
pub mod runtimes;
pub mod search;

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
    adapters::{Decision, Launch, LiveCmd, LiveHandle, PERMISSION_MODES},
    event::{By, Ev, ModelInfo, Out, Outcome, Queued, SlashCommand, ToolContent, TurnStatus},
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
    /// The commands and skills the agent offered last (see [`Ev::Commands`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<SlashCommand>,
    /// Whether the agent said it takes images (see [`Ev::Agent`]); its kind's caps until it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<bool>,
    /// The turn running now, to account for it (and go on with it) after a host restart.
    #[serde(default)]
    pub turn: Option<TurnMeta>,
    /// The runner's id of the agent process (an agent in a runner, see [`runner`]).
    #[serde(default)]
    pub runner_live: Option<String>,
    /// The runner's number of the last output of that agent recorded here.
    #[serde(default)]
    pub runner_seq: Option<u64>,
    /// The session's title; none: controllers show the first message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Where the title came from: `agent` (Claude Code summed up the first message) or `user` (a
    /// person named it; the agent's title no longer replaces it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_source: Option<String>,
    /// The first message, cut to [`FIRST_TEXT`] characters (a title before there is one, search).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_text: Option<String>,
    /// How the last turn ended, and why it failed (cut to 300 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<TurnStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reason: Option<String>,
    /// What waits for a person: `permission`, `question` or `plan` (the oldest open one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Pending>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Archived: `chat_sessions` lists it only when asked for archived sessions.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    /// Claude Code's permission mode (see [`adapters::PERMISSION_MODES`]); other agents have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// The session this one was forked from (see [`ChatManager::fork`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The parent's own conversation id when it was forked: the first agent process copies that
    /// conversation into a new one (and reports its id as `proto_session`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_from: Option<String>,
    /// The titles it had before, oldest first (at most [`PREVIOUS_TITLES`]), for search.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_titles: Vec<String>,
    /// The effort level the session runs with (see [`valid_effort`]); none: the agent's default.
    /// Like [`Meta::model`], what later messages use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The models the agent offered last (see [`Ev::Models`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelInfo>,
}

/// Characters of the first message kept in [`Meta::first_text`].
pub const FIRST_TEXT: usize = 200;

/// Earlier titles kept in [`Meta::previous_titles`].
pub const PREVIOUS_TITLES: usize = 10;

impl Meta {
    /// Sets the title (none: back to the first message), remembering the one it replaces.
    fn retitle(&mut self, title: Option<String>) {
        if let Some(old) = self.title.take().filter(|old| Some(old) != title.as_ref()) {
            self.previous_titles.retain(|t| *t != old);
            self.previous_titles.push(old);
            let extra = self.previous_titles.len().saturating_sub(PREVIOUS_TITLES);
            self.previous_titles.drain(..extra);
        }
        if let Some(t) = &title {
            self.previous_titles.retain(|p| p != t);
        }
        self.title = title;
    }
}

/// What an open request waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pending {
    Permission,
    Question,
    Plan,
}

/// A permission, plan or question the agent waits on a person for.
#[derive(Debug, Clone, Copy)]
struct OpenAsk {
    kind: Pending,
    /// When it came (the session's `seq`), so the oldest is what [`Meta::pending`] shows.
    seq: u64,
}

/// The running turn as `meta.json` keeps it: who sent the message and a hash of it, never the text
/// (the transcript has that).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnMeta {
    pub input_id: String,
    pub by: By,
    pub input_sha256: String,
    pub input_chars: usize,
    pub started_at: u64,
}

/// A message on its way to the agent.
#[derive(Debug, Clone)]
pub struct Input {
    pub id: String,
    pub text: String,
    pub by: By,
    /// Images sent with it (kept in memory only: the transcript records their names and sizes).
    pub images: Vec<Image>,
}

/// An image sent with a message.
#[derive(Debug, Clone)]
pub struct Image {
    pub name: String,
    /// One of [`IMAGE_TYPES`].
    pub mime: String,
    /// The image, base64 (standard alphabet, padded).
    pub data: Arc<str>,
    /// Its size decoded.
    pub bytes: usize,
}

/// Images a message may carry.
pub const MAX_IMAGES: usize = 4;
/// Largest image accepted, decoded: 3.75 MiB, which is 5 MiB in base64 (the most Anthropic's API
/// takes per image).
pub const MAX_IMAGE_BYTES: usize = 3_932_160;
/// The image types every agent here reads.
pub const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

impl Image {
    /// Checks one image of an `input` command: a type agents read, base64 that decodes, not too big.
    pub fn parse(name: &str, mime: &str, data: &str) -> Result<Self> {
        use base64::Engine as _;
        let mime = mime.trim().to_ascii_lowercase();
        if !IMAGE_TYPES.contains(&mime.as_str()) {
            bail!("'{mime}' is not an image type agents read (PNG, JPEG, GIF or WebP)");
        }
        if data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            bail!("the image '{name}' is larger than {} MiB", MAX_IMAGE_BYTES as f64 / 1048576.0);
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .map_err(|e| anyhow!("the image '{name}' is not valid base64: {e}"))?
            .len();
        let name: String = name.chars().filter(|c| !c.is_control()).take(120).collect();
        Ok(Self { name, mime, data: data.trim().into(), bytes })
    }

    /// What the transcript records of it.
    pub fn meta(&self) -> event::Attachment {
        event::Attachment { name: self.name.clone(), mime: self.mime.clone(), bytes: self.bytes }
    }
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
    /// The answer to a question, already claimed with [`Chat::claim`]: `answers` has every part's.
    Answer {
        qid: String,
        text: String,
        answers: Option<BTreeMap<String, String>>,
        by: By,
    },
    /// The answer to a permission or a plan, already claimed with [`Chat::claim`].
    Permit {
        rid: String,
        decision: adapters::Decision,
        message: Option<String>,
        mode: Option<String>,
        by: By,
    },
    /// Switch Claude Code's permission mode.
    SetMode {
        mode: String,
        by: By,
    },
    /// Use this model and effort level from the next message on (`None`: the agent's default).
    SetModel {
        model: Option<String>,
        effort: Option<String>,
        by: By,
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
    /// After a host restart: the agent still running in the runner, taken back, or `None` when it
    /// is gone (see [`ChatManager::adopt_all`]).
    Adopted(Option<Adoption>),
}

/// An agent process that outlived the host, ready for its session to drive again.
pub struct Adoption {
    pub handle: LiveHandle,
    pub out: mpsc::Receiver<Out>,
    /// Whether the turn the session thinks runs can still end (see [`runner::RunnerLink::adopt`]).
    pub turn_can_end: bool,
}

impl std::fmt::Debug for Adoption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Adoption").field("pid", &self.handle.pid).finish()
    }
}

/// Where the agent processes run.
pub enum Backend {
    /// In this process, as the user this host runs as.
    Local(detect::Detector),
    /// In a runner in the user's session (this host runs as SYSTEM).
    Runner(runner::RunnerLink),
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
    backend: Backend,
    chats: Mutex<BTreeMap<String, Arc<Chat>>>,
    tickets: Mutex<HashMap<String, Ticket>>,
    audit: mpsc::UnboundedSender<Value>,
    /// Paused by the person at a controller: no turn may run.
    paused: Mutex<Option<String>>,
    /// The commands an agent offered last, by agent and folder: what a new session shows in its `/`
    /// menu before its own agent process has started.
    commands: Mutex<HashMap<(String, String), Vec<SlashCommand>>>,
    /// The models each agent offered last (by agent id, newest list): what `chat_runtimes` and a
    /// new session show before an agent process has started.
    models: Mutex<HashMap<String, (u64, Vec<ModelInfo>)>>,
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
    /// The folder's files, for the `@` menu.
    pub files: files::FileCache,
    /// The permissions, plans and questions the agent waits on a person for, by id. The first
    /// answer takes one out ([`Chat::claim`]); later answers find it gone.
    open: Mutex<BTreeMap<String, OpenAsk>>,
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
        Self::with_backend(dir, config_path, Backend::Local(detect::Detector::default()))
    }

    /// [`new`](Self::new) with the agents running where `backend` says. With a runner, sessions
    /// whose agent was running are taken back from it in the background ([`Self::adopt_all`]).
    pub fn with_backend(
        dir: PathBuf,
        config_path: PathBuf,
        backend: Backend,
    ) -> (Self, mpsc::UnboundedReceiver<Value>) {
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
            backend,
            chats: Mutex::new(BTreeMap::new()),
            tickets: Mutex::new(HashMap::new()),
            audit,
            paused: Mutex::new(None),
            commands: Mutex::new(HashMap::new()),
            models: Mutex::new(HashMap::new()),
        });
        let manager = Self(inner);
        let pending = manager.load();
        if !pending.is_empty() {
            let m = manager.clone();
            tokio::spawn(async move { m.adopt_all(pending).await });
        }
        (manager, audit_rx)
    }

    /// Why agents cannot start on this host, if they cannot.
    pub fn blocked(&self) -> Option<String> {
        match &self.0.backend {
            Backend::Local(_) => detect::blocked(),
            Backend::Runner(_) => None,
        }
    }

    /// Before the host stops cleanly: a runner stops its agents and exits with it.
    pub async fn close(&self) {
        if let Backend::Runner(link) = &self.0.backend {
            link.close().await;
        }
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
        let mut v = match &self.0.backend {
            Backend::Local(d) => d.report(&custom, refresh).await,
            Backend::Runner(link) => match link.runtimes(&custom, refresh).await {
                Ok(v) => v,
                Err(e) => json!({"host": {"can_start": false, "reason": format!("{e:#}")}, "runtimes": []}),
            },
        };
        // The models each agent offered last here, for the new-session page's menu (none until an
        // agent has run once on this host).
        if let Some(list) = v["runtimes"].as_array_mut() {
            let cache = self.0.models.lock().unwrap();
            for r in list {
                if let Some((_, models)) = r["id"].as_str().and_then(|id| cache.get(id)) {
                    r["models"] = json!(models);
                }
            }
        }
        v["host"]["enabled"] = json!(self.enabled());
        v["host"]["max_live"] = json!(MAX_LIVE);
        v["host"]["config_error"] = json!(self.0.config_error);
        v
    }

    /// The sessions, newest first: with `archived`, only the archived ones or only the others.
    pub fn list(&self, archived: Option<bool>) -> Vec<Value> {
        let mut v: Vec<(u64, Value)> = self
            .all()
            .iter()
            .filter(|c| archived.is_none_or(|a| c.meta().archived == a))
            .map(|c| (c.meta().started_at, c.describe()))
            .collect();
        v.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        v.into_iter().map(|(_, v)| v).collect()
    }

    /// Searches what was said in every session here (see [`search`]): at most `limit` sessions,
    /// most recently active first, with the number of matching passages and excerpts of the last
    /// ones. Blocks while it reads; run it off the async threads.
    pub fn search(&self, query: &str, limit: usize) -> Value {
        let terms = search::terms(query);
        let mut chats: Vec<(Meta, PathBuf)> =
            self.all().iter().map(|c| (c.meta(), c.dir.join("events.jsonl"))).collect();
        chats.sort_by_key(|(m, _)| std::cmp::Reverse(m.last_activity));
        let started = Instant::now();
        let mut budget = search::Budget::new(search::TIME_BUDGET, search::READ_BUDGET);
        let (mut results, mut truncated, mut searched) = (Vec::new(), false, 0);
        if !terms.is_empty() {
            for (m, path) in chats {
                if results.len() >= limit {
                    break;
                }
                let found = search::transcript(&path, &terms, &mut budget);
                searched += 1;
                if found.hits > 0 {
                    results.push(json!({
                        "session": m.id, "title": m.title, "first_text": m.first_text, "runtime": m.runtime,
                        "name": m.name, "cwd": m.cwd, "state": m.state, "archived": m.archived,
                        "parent": m.parent, "last_activity": m.last_activity, "hits": found.hits,
                        "excerpts": found.excerpts,
                    }));
                }
                if found.truncated {
                    truncated = true;
                    break;
                }
            }
        }
        json!({
            "results": results, "truncated": truncated, "searched": searched,
            "elapsed_ms": started.elapsed().as_millis() as u64,
        })
    }

    /// The protocol of agent `id` (built in or custom).
    pub fn protocol_of(&self, id: &str) -> Option<Protocol> {
        runtimes::protocol_of(id, &self.custom())
    }

    /// Changes what a person may change of a session: its title (empty: back to the agent's or the
    /// first message), pinned, archived. Returns what `chat_sessions` shows of it now.
    pub fn update(
        &self,
        id: &str,
        title: Option<String>,
        pinned: Option<bool>,
        archived: Option<bool>,
        by: &Peer,
    ) -> Result<Value> {
        let chat = self.get(id)?;
        let title = title.map(|t| t.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string());
        if let Some(t) = &title
            && t.chars().count() > 200
        {
            bail!("a title has at most 200 characters");
        }
        chat.set_meta(|m| {
            if let Some(t) = &title {
                if t.is_empty() {
                    m.retitle(None);
                    m.title_source = None;
                } else {
                    m.retitle(Some(t.clone()));
                    m.title_source = Some("user".into());
                }
            }
            if let Some(p) = pinned {
                m.pinned = p;
            }
            if let Some(a) = archived {
                m.archived = a;
            }
        });
        if let Some(t) = title.filter(|t| !t.is_empty())
            && chat.state() != State::Ended
        {
            chat.emit(Ev::Title { title: t, source: "user".into() });
        }
        self.audit(json!({
            "kind": "chat_session", "action": "update", "chat": chat.id, "by": by.name, "principal": by.principal,
            "title": chat.meta().title_source.as_deref() == Some("user"), "pinned": pinned, "archived": archived,
        }));
        Ok(chat.describe())
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
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        &self,
        creator: Peer,
        runtime: &str,
        cwd: &str,
        model: Option<String>,
        hardened: bool,
        prompt: Option<String>,
        permission_mode: Option<String>,
        effort: Option<String>,
    ) -> Result<Arc<Chat>> {
        if let Some(e) = &effort
            && !valid_effort(e)
        {
            bail!("invalid effort level '{e}'");
        }
        self.create(creator, runtime, cwd, model, hardened, prompt, permission_mode, effort, None).await
    }

    /// Forks session `parent` for `creator`: a new session with its agent, folder, model and
    /// permission mode, whose agent copies the parent's conversation as it was last saved (a turn
    /// running now is left out) and goes on apart from it; the parent is not changed. Its
    /// transcript starts with the parent's finished turns. Only Claude Code (`--fork-session`)
    /// and Codex (`thread/fork`) can copy a conversation, and the parent's agent must have one
    /// (it has run a turn).
    pub async fn fork(&self, creator: Peer, parent: &str, prompt: Option<String>) -> Result<Arc<Chat>> {
        let p = self.get(parent)?;
        let pm = p.meta();
        if !self.protocol_of(&pm.runtime).is_some_and(adapters::can_fork) {
            bail!(
                "{} sessions cannot be forked: only Claude Code and Codex can copy a conversation into a new one",
                pm.name
            );
        }
        // A fork that has not run yet copies what it was forked from (the same conversation).
        let Some(from) = pm.proto_session.clone().or(pm.fork_from.clone()).filter(|_| pm.turns > 0) else {
            bail!("session '{}' has no conversation to fork yet: it has not run a turn", p.id);
        };
        let (runtime, cwd) = (pm.runtime.clone(), pm.cwd.clone());
        self.create(
            creator,
            &runtime,
            &cwd,
            pm.model.clone(),
            pm.hardened,
            prompt,
            pm.permission_mode,
            pm.effort,
            Some((p, from)),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn create(
        &self,
        creator: Peer,
        runtime: &str,
        cwd: &str,
        model: Option<String>,
        hardened: bool,
        prompt: Option<String>,
        permission_mode: Option<String>,
        effort: Option<String>,
        fork: Option<(Arc<Chat>, String)>,
    ) -> Result<Arc<Chat>> {
        if let Some(m) = &permission_mode
            && !PERMISSION_MODES.contains(&m.as_str())
        {
            bail!("unknown permission mode '{m}' (one of {})", PERMISSION_MODES.join(", "));
        }
        if let Some(e) = &self.0.config_error {
            bail!("coding agents are switched off on this host until its chat.toml is fixed: {e}");
        }
        if !self.enabled() {
            bail!("coding agents are switched off on this host (someone at it can run: ai-remote chat enable)");
        }
        // The folder is checked (and `~` expanded) by whoever runs the agent: as SYSTEM, `~` would
        // be SYSTEM's profile.
        let (rid, rname, program, cwd) = match &self.0.backend {
            Backend::Local(d) => {
                let resolved = d.resolve(runtime, &self.custom())?;
                let cwd_path = PathBuf::from(detect::expand_home(cwd.trim()));
                if !cwd_path.is_absolute() {
                    bail!("the working folder must be an absolute path, not '{cwd}'");
                }
                if !cwd_path.is_dir() {
                    bail!("the working folder {} does not exist on this host", cwd_path.display());
                }
                let cwd = dunce::simplified(&cwd_path).display().to_string();
                (resolved.id, resolved.name, resolved.program.display().to_string(), cwd)
            }
            Backend::Runner(link) => {
                let p = link.prepare(runtime, &self.custom(), cwd).await?;
                (p.id, p.name, p.program, p.cwd)
            }
        };
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
        // A fork's transcript starts with the parent's finished turns, as its agent's copy does.
        let copied = match &fork {
            Some((parent, _)) => {
                let (from, to) = (parent.dir.join("events.jsonl"), dir.join("events.jsonl"));
                match tokio::task::spawn_blocking(move || copy_transcript(&from, &to)).await {
                    Ok(Ok(turns)) => turns,
                    Ok(Err(e)) => {
                        let _ = std::fs::remove_dir_all(&dir);
                        bail!("cannot copy the transcript of session '{}': {e}", parent.id);
                    }
                    Err(e) => bail!("transcript copy failed: {e}"),
                }
            }
            None => 0,
        };
        let parent_meta = fork.as_ref().map(|(p, _)| p.meta());
        let by = By { device: creator.name.clone(), principal: creator.principal.clone() };
        // Only Claude Code has permission modes; its sessions always say which they run in.
        let permission_mode = (self.protocol_of(&rid) == Some(Protocol::ClaudeStreamJson))
            .then(|| permission_mode.unwrap_or_else(|| adapters::DEFAULT_PERMISSION_MODE.to_string()));
        let meta = Meta {
            id: id.clone(),
            runtime: rid.clone(),
            name: rname,
            version: None,
            cwd,
            model,
            hardened,
            by: by.clone(),
            started_at: now_ms(),
            state: State::Parked,
            proto_session: None,
            turns: copied,
            pid: None,
            pid_start: None,
            ended_at: None,
            end_reason: None,
            last_activity: now_ms(),
            commands: parent_meta.as_ref().map(|p| p.commands.clone()).unwrap_or_default(),
            images: parent_meta.as_ref().and_then(|p| p.images),
            turn: None,
            runner_live: None,
            runner_seq: None,
            title: None,
            title_source: None,
            first_text: parent_meta.as_ref().filter(|_| copied > 0).and_then(|p| p.first_text.clone()),
            last_status: None,
            last_reason: None,
            pending: None,
            pinned: false,
            archived: false,
            permission_mode: permission_mode.clone(),
            parent: fork.as_ref().map(|(p, _)| p.id.clone()),
            fork_from: fork.as_ref().map(|(_, from)| from.clone()),
            previous_titles: Vec::new(),
            effort: effort.clone(),
            models: Vec::new(),
        };
        let chat = self.insert(meta, creator, &dir, false)?;
        self.audit(json!({
            "kind": "chat_session", "action": if fork.is_some() { "fork" } else { "start" }, "chat": id,
            "runtime": rid, "program": program, "cwd": chat.meta().cwd, "by": by.device,
            "principal": by.principal, "hardened": hardened, "permission_mode": permission_mode,
            "parent": fork.as_ref().map(|(p, _)| p.id.clone()),
            "model": chat.meta().model, "effort": effort,
        }));
        if let Some(text) = prompt.filter(|t| !t.trim().is_empty()) {
            chat.send(Cmd::Input(Input { id: format!("u{}", &random_hex()[..8]), text, by, images: Vec::new() }));
        }
        self.prune();
        Ok(chat)
    }

    /// Registers a session and starts its driver. `adopting`: its agent may still run in the
    /// runner; the driver waits for [`Cmd::Adopted`] before it starts anything.
    fn insert(&self, meta: Meta, creator: Peer, dir: &Path, adopting: bool) -> Result<Arc<Chat>> {
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
            files: files::FileCache::new(matches!(self.0.backend, Backend::Local(_))),
            open: Mutex::new(BTreeMap::new()),
        });
        chat.save();
        self.0.chats.lock().unwrap().insert(chat.id.clone(), chat.clone());
        if !ended {
            tokio::spawn(drive(chat.clone(), Arc::downgrade(&self.0), rx, adopting));
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
                        input_id: None,
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

    /// The commands to offer for `chat` now: its own agent's last list, or what the same agent offered
    /// in the same folder (or, failing that, anywhere) in another session.
    pub fn commands_for(&self, chat: &Chat) -> Vec<SlashCommand> {
        let m = chat.meta();
        if !m.commands.is_empty() {
            return m.commands;
        }
        let cache = self.0.commands.lock().unwrap();
        cache
            .get(&(m.runtime.clone(), m.cwd.clone()))
            .or_else(|| cache.iter().find(|((r, _), _)| *r == m.runtime).map(|(_, v)| v))
            .cloned()
            .unwrap_or_default()
    }

    /// Whether `chat`'s agent takes images: what it said, or what its kind is known to do.
    pub fn takes_images(&self, chat: &Chat) -> bool {
        let m = chat.meta();
        if let Some(i) = m.images {
            return i;
        }
        runtimes::caps_of(&m.runtime, &self.custom()).is_some_and(|c| c.images)
    }

    /// The models to offer for `chat` now: its agent's last list, or what the same agent offered
    /// last in another session here.
    pub fn models_for(&self, chat: &Chat) -> Vec<ModelInfo> {
        let m = chat.meta();
        if !m.models.is_empty() {
            return m.models;
        }
        self.0.models.lock().unwrap().get(&m.runtime).map(|(_, v)| v.clone()).unwrap_or_default()
    }

    fn remember_models(&self, meta: &Meta) {
        if meta.models.is_empty() {
            return;
        }
        let mut cache = self.0.models.lock().unwrap();
        // The newest list wins (sessions load in no particular order).
        if cache.get(&meta.runtime).is_none_or(|(at, _)| *at <= meta.last_activity) {
            cache.insert(meta.runtime.clone(), (meta.last_activity, meta.models.clone()));
        }
    }

    fn remember_commands(&self, meta: &Meta) {
        if !meta.commands.is_empty() {
            self.0.commands.lock().unwrap().insert((meta.runtime.clone(), meta.cwd.clone()), meta.commands.clone());
        }
    }

    /// Loads the sessions on disk. A session whose agent was running when the host stopped is
    /// parked, and the processes it left are stopped (on Windows the Job Object already did) —
    /// unless the agent ran in a runner, which may still run it: those sessions are returned, to be
    /// taken back by [`Self::adopt_all`].
    fn load(&self) -> Vec<Arc<Chat>> {
        let mut pending = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.0.dir) else { return pending };
        let runner = matches!(self.0.backend, Backend::Runner(_));
        for entry in rd.flatten() {
            let dir = entry.path();
            let Ok(text) = std::fs::read_to_string(dir.join("meta.json")) else { continue };
            let Ok(mut meta) = serde_json::from_str::<Meta>(&text) else { continue };
            let claude = self.protocol_of(&meta.runtime) == Some(Protocol::ClaudeStreamJson);
            migrate(&mut meta, &dir.join("events.jsonl"), claude);
            self.remember_models(&meta);
            if runner && meta.state != State::Ended && meta.runner_live.is_some() && meta.pid.is_some() {
                let creator = Peer {
                    name: meta.by.device.clone(),
                    principal: meta.by.principal.clone(),
                    user_id: None,
                    level: None,
                    revoked: false,
                };
                match self.insert(meta, creator, &dir, true) {
                    Ok(chat) => pending.push(chat),
                    Err(e) => tracing::warn!("cannot load chat session {}: {e:#}", dir.display()),
                }
                continue;
            }
            let was_running = matches!(meta.state, State::Running | State::Interrupting | State::Starting);
            meta.turn = None;
            meta.runner_live = None;
            meta.runner_seq = None;
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
            self.remember_commands(&meta);
            match self.insert(meta, creator, &dir, false) {
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
        pending
    }

    /// Takes back the agents of `pending` sessions that still run in the runner (the host
    /// restarted while they ran); the others' processes are stopped and their sessions parked.
    /// Agents in the runner no session claims are stopped.
    async fn adopt_all(&self, pending: Vec<Arc<Chat>>) {
        let Backend::Runner(link) = &self.0.backend else { return };
        let mut lives = link.lives().await;
        for chat in pending {
            let meta = chat.meta();
            let found = meta
                .runner_live
                .as_deref()
                .and_then(|l| lives.iter().position(|i| i.live == l))
                .map(|i| lives.swap_remove(i));
            let adoption = match found {
                Some(info) => match link.adopt(info, meta.runner_seq.unwrap_or(0)).await {
                    Ok((handle, out, turn_can_end)) => Some(Adoption { handle, out, turn_can_end }),
                    Err(e) => {
                        tracing::warn!(chat = chat.id, "cannot take the agent back from the runner: {e:#}");
                        None
                    }
                },
                None => None,
            };
            if adoption.is_none()
                && let (Some(pid), Some(start)) = (meta.pid, meta.pid_start)
            {
                // Stopping is not running: SYSTEM may end what the user's agent left.
                let reaped = reap(pid, start);
                if reaped > 0 {
                    self.audit(
                        json!({"kind": "chat_admin", "action": "orphan_reaped", "chat": chat.id, "processes": reaped}),
                    );
                }
            }
            if adoption.is_some() {
                self.audit(json!({"kind": "chat_admin", "action": "reattached", "chat": chat.id}));
            }
            chat.send(Cmd::Adopted(adoption));
        }
        for left in lives {
            tracing::info!(live = left.live, "stopping an agent in the runner that no session claims");
            link.stop(&left.live).await;
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

    /// What `chat_sessions` shows. The workspace fields (title, pending, ...) are left out while
    /// they have no value, as an older host leaves them out.
    pub fn describe(&self) -> Value {
        let m = self.meta();
        let seq = self.log.lock().unwrap().seq;
        let mut v = json!({
            "id": m.id, "runtime": m.runtime, "name": m.name, "version": m.version, "cwd": m.cwd,
            "model": m.model, "state": m.state, "by": m.by, "started_at": m.started_at, "turns": m.turns,
            "last_activity": m.last_activity, "ended_at": m.ended_at, "end_reason": m.end_reason,
            "seq": seq, "last_seq": seq, "pinned": m.pinned, "archived": m.archived,
        });
        let extra = [
            ("title", m.title.map(Value::from)),
            ("title_source", m.title_source.map(Value::from)),
            ("first_text", m.first_text.map(Value::from)),
            ("last_status", m.last_status.map(|s| json!(s))),
            ("last_reason", m.last_reason.map(Value::from)),
            ("pending", m.pending.map(|p| json!(p))),
            ("permission_mode", m.permission_mode.map(Value::from)),
            ("effort", m.effort.map(Value::from)),
            ("branch", git::branch(Path::new(&m.cwd)).map(Value::from)),
            ("parent", m.parent.map(Value::from)),
            ("previous_titles", (!m.previous_titles.is_empty()).then(|| json!(m.previous_titles))),
        ];
        for (k, x) in extra {
            if let Some(x) = x {
                v[k] = x;
            }
        }
        v
    }

    /// Takes the open request `id` for an answer, if it is one of `kinds` and still open: the first
    /// answer gets it, and the request no longer counts as waiting.
    pub fn claim(&self, id: &str, kinds: &[Pending]) -> Option<Pending> {
        let kind = {
            let mut open = self.open.lock().unwrap();
            let kind = open.get(id).filter(|o| kinds.contains(&o.kind))?.kind;
            open.remove(id);
            kind
        };
        self.update_pending();
        Some(kind)
    }

    /// Whether `id` is an open request (of any kind).
    pub fn is_open(&self, id: &str) -> bool {
        self.open.lock().unwrap().contains_key(id)
    }

    /// Records in `meta.json` what waits for a person now (the oldest open request).
    fn update_pending(&self) {
        let pending = self.open.lock().unwrap().values().min_by_key(|o| o.seq).map(|o| o.kind);
        if self.meta().pending != pending {
            self.set_meta(|m| m.pending = pending);
        }
    }

    /// Opens a request (an event asking a person) and records it.
    fn open_ask(&self, id: &str, kind: Pending) {
        let seq = self.log.lock().unwrap().seq;
        self.open.lock().unwrap().insert(id.to_string(), OpenAsk { kind, seq });
        self.update_pending();
    }

    /// Settles every open request as cancelled (the turn ended, the process went away).
    fn cancel_open(&self) {
        let ids: Vec<String> = std::mem::take(&mut *self.open.lock().unwrap()).into_keys().collect();
        for id in ids {
            self.emit(Ev::Resolved { id, outcome: Outcome::Cancelled, by: None });
        }
        self.update_pending();
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

/// Fills what a `meta.json` written by an older host lacks: the first message and how the last
/// turn ended come from the transcript; a Claude Code session ran in bypass mode. Nothing is
/// changed for a session that has them (or never had a turn).
fn migrate(meta: &mut Meta, transcript: &Path, claude: bool) {
    if claude && meta.permission_mode.is_none() {
        meta.permission_mode = Some(adapters::DEFAULT_PERMISSION_MODE.to_string());
    }
    if meta.turns == 0 || (meta.first_text.is_some() && meta.last_status.is_some()) {
        return;
    }
    let Ok(f) = File::open(transcript) else { return };
    let mut first = None;
    let mut last = None;
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        // Only the lines that matter are parsed.
        if first.is_none() && line.contains("\"t\":\"user\"") {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                first = v["text"].as_str().map(|t| t.trim().chars().take(FIRST_TEXT).collect::<String>());
            }
        } else if line.contains("\"t\":\"turn_end\"")
            && let Ok(v) = serde_json::from_str::<Value>(&line)
        {
            last = serde_json::from_value::<TurnStatus>(v["status"].clone())
                .ok()
                .map(|s| (s, v["reason"].as_str().map(|r| event::cut(r, 300).0)));
        }
    }
    if meta.first_text.is_none() {
        meta.first_text = first.filter(|t| !t.is_empty());
    }
    if meta.last_status.is_none()
        && let Some((status, reason)) = last
    {
        meta.last_status = Some(status);
        meta.last_reason = reason.filter(|_| status == TurnStatus::Failed);
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

/// Copies the transcript `from` to `to` up to the end of its last finished turn (what follows, a
/// turn running now, is left out as the agent's copy of the conversation leaves it out), numbering
/// the events again from 1 and leaving out titles (a fork has its own). Returns the number of the
/// last turn copied. Reads and writes one line at a time.
fn copy_transcript(from: &Path, to: &Path) -> std::io::Result<u32> {
    use std::io::{BufWriter, Read};
    // The first pass finds where the last finished turn ends.
    let mut end = 0u64;
    let mut pos = 0u64;
    let mut turns = 0u32;
    let mut reader = BufReader::new(File::open(from)?);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        pos += n as u64;
        if line.contains("\"t\":\"turn_end\"") && line.ends_with('\n') {
            end = pos;
            if let Some(t) = serde_json::from_str::<Value>(&line).ok().and_then(|v| v["turn"].as_u64()) {
                turns = t as u32;
            }
        }
    }
    let mut reader = BufReader::new(File::open(from)?.take(end));
    let file = OpenOptions::new().create_new(true).write(true).open(to)?;
    restrict(&file, to);
    let mut out = BufWriter::new(file);
    let mut seq = 0u64;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let Some(rest) = line.strip_prefix("{\"seq\":") else { continue };
        let Some(i) = rest.find(',') else { continue };
        if line.contains("\"t\":\"title\"") {
            continue;
        }
        seq += 1;
        write!(out, "{{\"seq\":{seq},{}", &rest[i + 1..])?;
    }
    out.flush()?;
    Ok(turns)
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
async fn drive(chat: Arc<Chat>, manager: Weak<Inner>, mut rx: mpsc::UnboundedReceiver<Cmd>, adopting: bool) {
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
        adopting,
        relaunch: false,
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
    info: TurnMeta,
    started: Instant,
    last_event: Instant,
    interrupt_deadline: Option<Instant>,
    /// A message to send first once this turn has stopped.
    then: Option<Input>,
    tools: BTreeMap<&'static str, u32>,
    /// Messages that joined this turn.
    steers: u32,
    /// Question requests waiting for the user: the turn is quiet because of them, not stuck.
    asking: HashSet<String>,
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
    /// Waiting to learn whether the runner still runs this session's agent: nothing starts.
    adopting: bool,
    /// The process runs with settings it cannot take while it runs (Claude Code's effort level):
    /// it restarts, resuming the conversation, as soon as no turn runs.
    relaunch: bool,
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
                    && l.tx
                        .send(LiveCmd::Steer {
                            id: input.id.clone(),
                            text: input.text.clone(),
                            images: input.images.clone(),
                        })
                        .await
                        .is_ok()
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
                self.chat.emit(Ev::Notice { input_id: None, level: "info", code: "steer_queued", text });
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
            Cmd::Answer { qid, text, answers, by } => match &self.live {
                Some(l) => {
                    let _ = l.tx.send(LiveCmd::Answer { qid: qid.clone(), text, answers }).await;
                    self.chat.emit(Ev::Resolved { id: qid.clone(), outcome: Outcome::Answered, by: Some(by.clone()) });
                    self.audit_answer("answer", &qid, None, &by);
                }
                None => self.chat.emit(Ev::Notice {
                    input_id: None,
                    level: "warn",
                    code: "no_question",
                    text: "nothing is waiting for an answer".into(),
                }),
            },
            Cmd::Permit { rid, decision, message, mode, by } => match &self.live {
                Some(l) => {
                    let outcome = match decision {
                        Decision::Once => Outcome::Allowed,
                        Decision::Session => Outcome::AllowedSession,
                        Decision::Deny => Outcome::Denied,
                    };
                    let _ = l.tx.send(LiveCmd::Permit { rid: rid.clone(), decision, message, mode }).await;
                    self.chat.emit(Ev::Resolved { id: rid.clone(), outcome, by: Some(by.clone()) });
                    self.audit_answer("permit", &rid, Some(outcome), &by);
                }
                None => self.chat.emit(Ev::Notice {
                    input_id: None,
                    level: "warn",
                    code: "permission_expired",
                    text: "the agent is no longer waiting for that answer".into(),
                }),
            },
            Cmd::SetMode { mode, by } => self.set_mode(mode, by).await,
            Cmd::SetModel { model, effort, by } => self.set_model(model, effort, by).await,
            Cmd::Dequeue { id } => {
                self.queue.retain(|i| i.id != id);
                self.chat.emit_state(&self.queue);
            }
            Cmd::Park { reason } => {
                if self.live.is_some() {
                    self.stop_process(&reason).await;
                }
            }
            Cmd::Adopted(a) => self.adopted(a).await,
            Cmd::End { reason } => {
                self.adopting = false;
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

    /// Records who answered a permission, plan or question, never what they wrote.
    fn audit_answer(&self, action: &str, id: &str, outcome: Option<Outcome>, by: &By) {
        if let Some(m) = self.manager() {
            m.audit(json!({
                "kind": "chat_permission", "action": action, "chat": self.chat.id, "id": id, "outcome": outcome,
                "by": by.device, "principal": by.principal,
            }));
        }
    }

    /// Switches Claude Code's permission mode: the running agent is told; a session without a
    /// process records it for the next one.
    async fn set_mode(&mut self, mode: String, by: By) {
        let meta = self.chat.meta();
        let claude = self.manager().and_then(|m| m.protocol_of(&meta.runtime)) == Some(Protocol::ClaudeStreamJson);
        if !claude || !PERMISSION_MODES.contains(&mode.as_str()) {
            let text = if claude {
                format!("'{mode}' is not a permission mode (one of {})", PERMISSION_MODES.join(", "))
            } else {
                format!("{} has no permission modes", meta.name)
            };
            self.chat.emit(Ev::Notice { input_id: None, level: "warn", code: "mode_rejected", text });
            return;
        }
        if let Some(m) = self.manager() {
            m.audit(json!({
                "kind": "chat_session", "action": "mode", "chat": self.chat.id, "mode": mode,
                "by": by.device, "principal": by.principal,
            }));
        }
        match &self.live {
            // The agent says when it switched (a `mode` event).
            Some(l) => {
                let _ = l.tx.send(LiveCmd::SetMode { mode }).await;
            }
            None => {
                if meta.permission_mode.as_deref() != Some(mode.as_str()) {
                    self.chat.set_meta(|m| m.permission_mode = Some(mode.clone()));
                    self.chat.emit(Ev::Mode { mode });
                }
            }
        }
    }

    /// Switches the model and effort level the next messages run with (`None`: the agent's default).
    /// Codex takes both with its next turn and ACP agents a model from their config options;
    /// Claude Code switches models in the running process (it says when, [`Out::ModelSet`]) but
    /// takes an effort level only at start, so for another level its process restarts, resuming the
    /// conversation, once no turn runs. A session without a process records them for the next one.
    async fn set_model(&mut self, model: Option<String>, effort: Option<String>, by: By) {
        let meta = self.chat.meta();
        let model = model.map(|m| m.trim().to_string()).filter(|m| !m.is_empty());
        let effort = effort.map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty());
        let bad = match (&model, &effort) {
            (Some(m), _) if !valid_token(m) => Some(format!("'{m}' is not a model name")),
            (_, Some(e)) if !valid_effort(e) => Some(format!("'{e}' is not an effort level")),
            _ => None,
        };
        if let Some(text) = bad {
            self.chat.emit(Ev::Notice { input_id: None, level: "warn", code: "model_rejected", text });
            return;
        }
        let Some(manager) = self.manager() else { return };
        let protocol = manager.protocol_of(&meta.runtime);
        // A level the model does not take is not kept (NS-33), as far as the agent said which it takes;
        // ACP agents take none here.
        let effort = effort.filter(|e| {
            protocol != Some(Protocol::Acp) && takes_effort(&manager.models_for(&self.chat), model.as_deref(), e)
        });
        if model == meta.model && effort == meta.effort {
            self.chat.emit(Ev::Model { model, effort });
            return;
        }
        manager.audit(json!({
            "kind": "chat_session", "action": "model", "chat": self.chat.id, "model": model, "effort": effort,
            "by": by.device, "principal": by.principal,
        }));
        let Some(live) = &self.live else {
            self.record_model(model, effort);
            return;
        };
        match protocol {
            // Claude Code says when it switched (or why not); the session records it then.
            Some(Protocol::ClaudeStreamJson) if effort == meta.effort => {
                let _ = live.tx.send(LiveCmd::SetModel { model, effort }).await;
            }
            Some(Protocol::ClaudeStreamJson) => {
                self.record_model(model, effort);
                self.relaunch = true;
                if self.turn.is_none() {
                    self.restart().await;
                }
            }
            Some(Protocol::Acp) => {
                let _ = live.tx.send(LiveCmd::SetModel { model, effort: None }).await;
            }
            // Codex: from its next turn on.
            _ => {
                let _ = live.tx.send(LiveCmd::SetModel { model: model.clone(), effort: effort.clone() }).await;
                self.record_model(model, effort);
            }
        }
    }

    /// Records the model and effort level in use and tells every viewer.
    fn record_model(&self, model: Option<String>, effort: Option<String>) {
        self.chat.set_meta(|m| {
            m.model = model.clone();
            m.effort = effort.clone();
        });
        self.chat.emit(Ev::Model { model, effort });
    }

    /// Restarts the agent process with the session's settings now (nothing runs): it resumes the
    /// conversation, and messages wait for it as for any start.
    async fn restart(&mut self) {
        self.relaunch = false;
        if self.live.is_none() {
            return;
        }
        self.stop_process("restarting with new settings").await;
        let Some(manager) = self.manager() else { return };
        if let Err(e) = self.launch(&manager).await {
            self.chat.emit(Ev::Notice { input_id: None, level: "error", code: "start_failed", text: format!("{e:#}") });
            self.chat.set_state(State::Parked, &self.queue);
        }
    }

    /// Queues a message and starts it if nothing runs.
    async fn enqueue(&mut self, input: Input) {
        if self.queue.len() >= MAX_QUEUE {
            self.chat.emit(Ev::Notice {
                input_id: Some(input.id.clone()),
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
                let attachments = input.images.iter().map(Image::meta).collect();
                self.chat.emit(Ev::User {
                    input_id: input.id,
                    by: input.by,
                    text: input.text,
                    steer: true,
                    attachments,
                });
                if let Some(t) = self.turn.as_mut() {
                    t.steers += 1;
                }
                self.chat.set_meta(|m| m.last_activity = now_ms());
                self.chat.emit_state(&self.queue);
            }
            Out::SteerLate { .. } => {
                self.chat.emit(Ev::Notice {
                    input_id: None,
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
                    input_id: None,
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
        if self.turn.is_some() || self.queue.is_empty() || self.adopting {
            return;
        }
        let Some(manager) = self.manager() else { return };
        if let Some(by) = manager.0.paused.lock().unwrap().clone() {
            self.chat.emit(Ev::Notice {
                input_id: None,
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
                    input_id: None,
                    level: "warn",
                    code: "too_many",
                    text: format!("{MAX_LIVE} agents are already running here; the message waits until one stops"),
                });
                return;
            }
            if let Err(e) = self.launch(&manager).await {
                self.chat.emit(Ev::Notice {
                    input_id: None,
                    level: "error",
                    code: "start_failed",
                    text: format!("{e:#}"),
                });
                self.chat.set_state(State::Parked, &self.queue);
                // The message stays queued; it goes out with the next attempt.
                return;
            }
        }
        let input = self.queue.pop_front().expect("not empty");
        let Some(live) = &self.live else { return };
        // A message the agent already has starts its turn by itself.
        if !self.delivered.remove(&input.id)
            && live.tx.send(LiveCmd::Prompt { text: input.text.clone(), images: input.images.clone() }).await.is_err()
        {
            self.queue.push_front(input);
            return;
        }
        self.chat.emit(Ev::User {
            input_id: input.id.clone(),
            by: input.by.clone(),
            text: input.text.clone(),
            steer: false,
            attachments: input.images.iter().map(Image::meta).collect(),
        });
        self.chat.emit(Ev::TurnStart {});
        let info = TurnMeta {
            input_id: input.id.clone(),
            by: input.by.clone(),
            input_sha256: data_encoding::HEXLOWER.encode(&Sha256::digest(input.text.as_bytes())),
            input_chars: input.text.chars().count(),
            started_at: now_ms(),
        };
        let meta = self.chat.meta();
        if meta.turns == 0 && meta.title.is_none() && !input.text.trim().is_empty() {
            // The agent names the session after its first message (Claude Code; others ignore it).
            let _ = live.tx.send(LiveCmd::GenerateTitle { description: input.text.clone() }).await;
        }
        let first_text = (meta.first_text.is_none() && !input.text.trim().is_empty())
            .then(|| input.text.trim().chars().take(FIRST_TEXT).collect::<String>());
        self.chat.set_meta(|m| {
            m.turns += 1;
            m.last_activity = now_ms();
            m.turn = Some(info.clone());
            if first_text.is_some() {
                m.first_text = first_text;
            }
        });
        self.turn = Some(Turn {
            info,
            started: Instant::now(),
            last_event: Instant::now(),
            interrupt_deadline: None,
            asking: HashSet::new(),
            then: None,
            tools: BTreeMap::new(),
            steers: 0,
        });
        self.chat.set_state(State::Running, &self.queue);
    }

    async fn launch(&mut self, manager: &ChatManager) -> Result<()> {
        let meta = self.chat.meta();
        let (handle, out, live, steer) = match &manager.0.backend {
            Backend::Local(d) => {
                let resolved = d.resolve(&meta.runtime, &manager.custom())?;
                let (handle, out) = adapters::launch(Launch {
                    resolved: &resolved,
                    cwd: Path::new(&meta.cwd),
                    model: meta.model.as_deref(),
                    hardened: meta.hardened,
                    resume: meta.proto_session.as_deref().or(meta.fork_from.as_deref()),
                    fork: meta.proto_session.is_none() && meta.fork_from.is_some(),
                    permission_mode: meta.permission_mode.as_deref(),
                    effort: meta.effort.as_deref(),
                })?;
                (handle, out, None, resolved.caps.steer)
            }
            Backend::Runner(link) => {
                let live = format!("{}-{}", meta.id, &random_hex()[..8]);
                let (handle, out) = link
                    .launch(
                        &live,
                        &meta.runtime,
                        &manager.custom(),
                        &meta.cwd,
                        meta.model.as_deref(),
                        meta.hardened,
                        meta.proto_session.as_deref().or(meta.fork_from.as_deref()),
                        meta.proto_session.is_none() && meta.fork_from.is_some(),
                        meta.permission_mode.as_deref(),
                        meta.effort.as_deref(),
                    )
                    .await?;
                let steer = runtimes::caps_of(&meta.runtime, &manager.custom()).is_some_and(|c| c.steer);
                (handle, out, Some(live), steer)
            }
        };
        self.chat.set_meta(|m| {
            m.pid = Some(handle.pid);
            m.pid_start = Some(handle.start);
            m.runner_live = live;
            m.runner_seq = None;
        });
        if meta.proto_session.is_some() {
            self.chat.emit(Ev::Notice {
                input_id: None,
                level: "info",
                code: "resumed",
                text: format!("{} restarted and resumes the conversation", meta.name),
            });
        }
        self.steer = steer;
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
            match &out {
                Out::Ev(Ev::Question { qid, request, .. }) => {
                    t.asking.insert(request.clone().unwrap_or_else(|| qid.clone()));
                }
                Out::Ev(Ev::QuestionDone { request, .. }) => {
                    t.asking.remove(request);
                }
                _ => {}
            }
        }
        match out {
            Out::Ev(Ev::Agent { images }) => {
                if self.chat.meta().images != Some(images) {
                    self.chat.set_meta(|m| m.images = Some(images));
                    self.chat.emit(Ev::Agent { images });
                }
            }
            Out::Ev(Ev::Commands { mut commands }) => {
                // Only a list that changed goes into the transcript (an agent sends it at every start).
                commands.truncate(event::MAX_COMMANDS);
                if self.chat.meta().commands != commands {
                    self.chat.set_meta(|m| m.commands = commands.clone());
                    if let Some(m) = self.manager() {
                        m.remember_commands(&self.chat.meta());
                    }
                    self.chat.emit(Ev::Commands { commands });
                }
            }
            Out::Ev(Ev::Models { mut models }) => {
                models.truncate(event::MAX_MODELS);
                if self.chat.meta().models != models {
                    self.chat.set_meta(|m| m.models = models.clone());
                    if let Some(m) = self.manager() {
                        m.remember_models(&self.chat.meta());
                    }
                    self.chat.emit(Ev::Models { models });
                }
            }
            Out::ModelSet { model, effort } => self.record_model(model, effort),
            Out::Ev(Ev::Title { title, source }) => {
                // A name a person gave is not replaced by the agent's.
                if self.chat.meta().title_source.as_deref() != Some("user") {
                    self.chat.set_meta(|m| {
                        m.retitle(Some(title.clone()));
                        m.title_source = Some(source.clone());
                    });
                    self.chat.emit(Ev::Title { title, source });
                }
            }
            Out::Ev(Ev::Mode { mode }) => {
                if self.chat.meta().permission_mode.as_deref() != Some(mode.as_str()) {
                    self.chat.set_meta(|m| m.permission_mode = Some(mode.clone()));
                    self.chat.emit(Ev::Mode { mode });
                }
            }
            Out::Ev(Ev::Resolved { id, outcome, by }) => {
                // The agent stopped waiting: settled now, unless an answer took it first.
                if self.chat.claim(&id, &[Pending::Permission, Pending::Question, Pending::Plan]).is_some() {
                    self.chat.emit(Ev::Resolved { id, outcome, by });
                }
            }
            Out::Ev(ev @ (Ev::Permission { .. } | Ev::PlanReview { .. } | Ev::Question { .. })) => {
                let (id, kind) = match &ev {
                    Ev::Permission { rid, .. } => (rid.clone(), Pending::Permission),
                    Ev::PlanReview { rid, .. } => (rid.clone(), Pending::Plan),
                    Ev::Question { qid, .. } => (qid.clone(), Pending::Question),
                    _ => unreachable!(),
                };
                // Open before anyone sees it, so the first answer finds it.
                self.chat.open_ask(&id, kind);
                self.chat.emit(ev);
            }
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
            Out::Mark(n) => {
                // Recorded first, then acknowledged: the runner forgets only what is on disk here.
                self.chat.set_meta(|m| m.runner_seq = Some(n));
                if let Some(ack) = self.live.as_ref().and_then(|l| l.ack.as_ref()) {
                    let _ = ack.send(n);
                }
            }
        }
        None
    }

    /// The answer to whether the agent outlived the host (see [`ChatManager::adopt_all`]).
    async fn adopted(&mut self, a: Option<Adoption>) {
        if !self.adopting {
            return; // ended meanwhile; dropping the handle stops the agent
        }
        self.adopting = false;
        let meta = self.chat.meta();
        match a {
            Some(a) => {
                self.live = Some(a.handle);
                self.out = Some(a.out);
                self.idle_since = Instant::now();
                self.steer = self
                    .manager()
                    .is_some_and(|m| runtimes::caps_of(&meta.runtime, &m.custom()).is_some_and(|c| c.steer));
                self.chat.emit(Ev::Notice {
                    input_id: None,
                    level: "info",
                    code: "reattached",
                    text: format!("the host restarted; {} kept running and is connected again", meta.name),
                });
                if meta.turn.is_some() && !a.turn_can_end {
                    // The host died while handing the message over; the agent never got it.
                    self.chat.emit(Ev::TurnEnd {
                        status: TurnStatus::Failed,
                        final_text: None,
                        duration_ms: 0,
                        reason: Some("the host restarted before the message reached the agent; send it again".into()),
                    });
                    self.chat.set_meta(|m| m.turn = None);
                } else if let Some(info) = meta.turn {
                    // The turn goes on; what it reported meanwhile follows from the runner.
                    let ago = Duration::from_millis(now_ms().saturating_sub(info.started_at));
                    let started = Instant::now().checked_sub(ago).unwrap_or_else(Instant::now);
                    self.turn = Some(Turn {
                        info,
                        started,
                        last_event: Instant::now(),
                        interrupt_deadline: None,
                        asking: HashSet::new(),
                        then: None,
                        tools: BTreeMap::new(),
                        steers: 0,
                    });
                }
                let state = if self.turn.is_some() { State::Running } else { State::Idle };
                self.chat.set_state(state, &self.queue);
            }
            None => {
                self.chat.set_meta(|m| {
                    m.pid = None;
                    m.pid_start = None;
                    m.runner_live = None;
                    m.runner_seq = None;
                });
                if meta.turn.is_some() {
                    self.chat.emit(Ev::TurnEnd {
                        status: TurnStatus::Failed,
                        final_text: None,
                        duration_ms: 0,
                        reason: Some("the host restarted during this turn".into()),
                    });
                    self.chat.set_meta(|m| m.turn = None);
                }
                self.chat.set_state(State::Parked, &self.queue);
            }
        }
        self.next_turn().await;
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
                exit_code: None,
                duration_ms: None,
            };
            return (ev, held);
        }
        let (is_thought, id, mut text, phase) = match first {
            Ev::Message { id, text, phase, .. } => (false, id, text, phase),
            Ev::Thought { id, text, .. } => (true, id, text, None),
            other => return (other, None),
        };
        let mut held = None;
        if let Some(rx) = self.out.as_mut() {
            while text.len() < 16 * 1024 {
                match rx.try_recv() {
                    Ok(Out::Ev(Ev::Message { id: i, text: t, delta: true, .. })) if !is_thought && i == id => {
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
        let ev = if is_thought {
            Ev::Thought { id, text, delta: true }
        } else {
            Ev::Message { id, text, delta: true, phase }
        };
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
        // What the turn still asked about falls away with it.
        self.chat.cancel_open();
        self.chat.emit(Ev::TurnEnd { status, final_text, duration_ms, reason: reason.clone() });
        let last_reason = reason.as_deref().filter(|_| status == TurnStatus::Failed).map(|r| event::cut(r, 300).0);
        self.chat.set_meta(|m| {
            m.last_activity = now_ms();
            m.turn = None;
            m.last_status = Some(status);
            m.last_reason = last_reason;
        });
        if let Some(m) = self.manager() {
            let info = &turn.info;
            m.audit(json!({
                "kind": "chat_turn", "chat": self.chat.id, "runtime": self.chat.meta().runtime,
                "turn": self.chat.log.lock().unwrap().turn, "by": info.by.device, "principal": info.by.principal,
                "input_sha256": info.input_sha256, "input_chars": info.input_chars, "outcome": status,
                "duration_ms": duration_ms, "tools": turn.tools, "steers": turn.steers, "reason": reason,
            }));
        }
        self.idle_since = Instant::now();
        let state = if self.live.is_some() { State::Idle } else { State::Parked };
        self.chat.set_state(state, &self.queue);
        if let Some(then) = turn.then {
            self.queue.push_front(then);
        }
        if self.relaunch {
            // Boxed: restarting stops the process, which may end a turn, which may restart.
            Box::pin(self.restart()).await;
        }
        self.next_turn().await;
    }

    /// The agent process is gone: the turn (if any) failed, the session is parked.
    async fn process_gone(&mut self, why: String) {
        self.live = None;
        self.out = None;
        self.requeue_unanswered();
        self.chat.cancel_open();
        self.chat.set_meta(|m| {
            m.pid = None;
            m.pid_start = None;
            m.runner_live = None;
        });
        if self.turn.is_some() {
            let interrupted = self.turn.as_ref().is_some_and(|t| t.interrupt_deadline.is_some());
            let status = if interrupted { TurnStatus::Interrupted } else { TurnStatus::Failed };
            self.end_turn(status, None, Some(why)).await;
        } else {
            self.chat.emit(Ev::Notice { input_id: None, level: "info", code: "stopped", text: why });
            self.chat.set_state(State::Parked, &self.queue);
        }
    }

    /// Stops the agent process (closing its input first) and parks the session.
    async fn stop_process(&mut self, reason: &str) {
        let Some(live) = self.live.take() else { return };
        self.out = None;
        self.requeue_unanswered();
        self.chat.cancel_open();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        if live.tx.send(LiveCmd::Shutdown { done: done_tx }).await.is_ok() {
            let _ = tokio::time::timeout(Duration::from_secs(15), done_rx).await;
        }
        drop(live);
        self.chat.set_meta(|m| {
            m.pid = None;
            m.pid_start = None;
            m.runner_live = None;
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
                    input_id: None,
                    level: "warn",
                    code: "killed",
                    text: "the agent did not stop in time; its process was ended".into(),
                });
                self.stop_process("stopped by force after an interrupt").await;
                return;
            }
            // A turn waiting for the user's answer is quiet, not stuck.
            if t.interrupt_deadline.is_none() && t.asking.is_empty() && t.last_event.elapsed() >= QUIET_TURN {
                self.chat.emit(Ev::Notice {
                    input_id: None,
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
        Ev::ToolUpdate { call_id, status: None, content, truncated: false, exit_code: None, duration_ms: None } => {
            match content.as_slice() {
                [ToolContent::Text { text }] => Some((call_id, text)),
                _ => None,
            }
        }
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

/// Whether `model` (`None`: the agent's default) takes the effort level `effort`, as far as
/// `models` (what the agent offered) says: a model it lists with no levels takes none; a model it
/// does not list is not known, so any level goes.
pub fn takes_effort(models: &[ModelInfo], model: Option<&str>, effort: &str) -> bool {
    let entry = match model {
        Some(m) => models.iter().find(|x| x.id == m),
        None => models.iter().find(|x| x.default),
    };
    entry.is_none_or(|x| x.efforts.iter().any(|e| e == effort))
}

/// An effort level as agents name them (`low` … `max`, Codex's `minimal`, `none`, `ultra`): lower
/// case letters, digits, `-` and `_`. Which levels a model takes is the agent's to say.
pub fn valid_effort(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 16
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
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

#[cfg(test)]
mod image_tests {
    use super::*;

    #[test]
    fn images_must_be_a_type_agents_read_valid_and_small_enough() {
        let ok = Image::parse("shot.PNG", "IMAGE/PNG", "iVBORw0KGgo=").unwrap();
        assert_eq!((ok.mime.as_str(), ok.bytes), ("image/png", 8));
        assert!(Image::parse("a.svg", "image/svg+xml", "PHN2Zz4=").is_err());
        assert!(Image::parse("a.png", "image/png", "not base64!").is_err());
        let big = "A".repeat(MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4);
        assert!(Image::parse("a.png", "image/png", &big).unwrap_err().to_string().contains("larger"));
        assert_eq!(
            serde_json::to_value(ok.meta()).unwrap(),
            json!({"name": "shot.PNG", "mime": "image/png", "bytes": 8})
        );
    }
}
