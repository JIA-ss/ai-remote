//! `app/ai-remote/input/1`: a person's mouse and keyboard, live, while they drive from a paired
//! device, and the text clipboard both ways. The `mouse` / `keyboard` tools (one validated action
//! per call, through the gate and the audit log) stay for agents; a person moving a pointer needs
//! an event every few milliseconds, which a tool call per event cannot give.
//!
//! The controller opens a bi-stream and writes one JSON line, `{"clipboard":true}` (every field
//! optional). The host answers
//! `{"ok":true,"os":"windows"|"macos"|"other","clipboard":bool,"clipboard_write":bool}` (whether it
//! reports its clipboard, and whether it accepts `clip` lines) or `{"ok":false,"error":".."}`. Then
//! both sides write JSON lines until either closes:
//!
//! controller → host
//! - `{"m":[x,y]}` put the pointer on a screen point (screen coordinates: physical pixels on
//!   Windows, points on macOS, as in `system_info`'s monitors and the live view's `screen`);
//! - `{"b":"left"|"right"|"middle","down":true|false}` press or release a button where the
//!   pointer is;
//! - `{"w":[h,v]}` wheel notches, positive is right / down;
//! - `{"k":"<name>","down":true|false}` press or release a key by the `keyboard` tool's names
//!   (`ctrl`, `shift`, `alt`, `win` / `cmd`, `a`, `enter`, `f5`, ...; a single character means
//!   that key on the host's layout); a press of a key already down is an auto-repeat;
//! - `{"t":"text"}` type text (what an input method composed);
//! - `{"clip":"text"}` replace the host's clipboard text;
//! - `{"release":true}` release every key and button the stream holds;
//! - any other line (`{"hb":1}`) only says the controller is still there: with nothing at all for
//!   [`QUIET`], the host lets go of everything the stream holds (an app that crashed or lost its
//!   network must not leave a key down for the transport's idle timeout).
//!
//! host → controller
//! - `{"clip":"text"}` the host's clipboard text changed (polled; not what the controller set, and not
//!   what it held when the stream opened: only changes go either way, so neither side's clipboard
//!   is replaced just by taking over);
//! - `{"clip_files":["path",..]}` the files on the host's clipboard changed (their paths on the host;
//!   an empty list when it no longer holds files);
//! - `{"err":"why"}` an event was refused or failed (the others still run);
//! - `{"blocked":"why"}` / `{"blocked":null}` input is paused (an approval prompt is open, the
//!   desktop is not usable) and resumed; held keys and buttons are let go when it pauses.
//!
//! Admission is the gate's for `mouse` and `keyboard` with the person's standing: the device must
//! have taken over (it drives), have level `control` (or the owner's bypass), and the host's policy
//! for both tools must be `allow` (a `deny` refuses the stream; `ask` refuses it too, since asking
//! the owner about every pointer move is no approval at all, and the app then falls back to one
//! call per action, which does ask). Clipboard sync follows the policy for `clipboard_read` (the
//! host's clipboard is reported) and `clipboard_write` (`clip` lines are accepted), each `allow`
//! only. All of this is checked again every second while the stream runs: handing back, a pause,
//! another device taking over, a lower level, a bypass that ended or a policy change ends the
//! stream (or its clipboard sync) within that second, after letting go of everything it holds.
//! While the stream runs every press is checked like a tool's (no approval prompt open, desktop
//! usable, never the host's own windows); moves are not. Refused chords (`keys::forbidden`) are
//! refused here too; Ctrl+Alt+Del goes through the service where there is one (the keys held for
//! it are let go first). A character no key of the host's layout types is entered as text (unless
//! a shortcut modifier is held). The clipboard is left alone while the desktop is locked or a
//! secure desktop is up (nobody there expects it to change). Everything held is released when the
//! stream ends, however it ends. The audit log records the stream (`input_start`, `input_end` with
//! counts, `input_refused`), not the events: what a person types is theirs.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc as std_mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use ar_core::{Level, config::Action};
use ar_net::{
    Peer,
    iroh::{
        endpoint::{Connection, RecvStream, SendStream},
        protocol::{AcceptError, ProtocolHandler},
    },
    refusal::{CLOSE_NO_ACCESS, REASON_NO_LEVEL, REASON_NOT_PAIRED},
};
use enigo::{Axis, Button, Direction, Key};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::mpsc,
};

use crate::{
    HostState,
    approval::OpenPrompts,
    platform::{self, InputTarget, InputTargetCheck},
    policy,
    tools::input::{
        inject::{Backend, EnigoBackend},
        keys::{self, KeyName, Modifier, Os, Pressed},
    },
    turns::Driver,
};

/// Longest clipboard text sent either way, in bytes.
const MAX_CLIP: usize = 1 << 20;
/// Longest line accepted from the controller: a `clip` line of `MAX_CLIP` bytes whose every byte
/// JSON escapes to six (`\u0000`). A longer line is skipped with an error; the stream goes on.
const MAX_LINE: usize = 6 * MAX_CLIP + 64;
/// Most characters one `t` event types.
const MAX_TEXT: usize = 10_000;
/// Most wheel notches one event scrolls on an axis.
const MAX_SCROLL: i64 = 50;
/// Events waiting for the injection thread before pointer moves and wheel are dropped (a stalled
/// desktop must not bank minutes of movement).
const MAX_PENDING: usize = 2048;
/// Live input streams at once on this host (each has a thread and a clipboard poller).
const MAX_STREAMS: usize = 4;
/// How often the host's clipboard is checked for changes.
const CLIP_POLL: Duration = Duration::from_millis(400);
/// How often, while only moving, the Windows input desktop is followed and the desktop checked.
const RECHECK: Duration = Duration::from_millis(500);
/// How often the stream's standing (turn, level, bypass, policy) is checked again.
const STANDING: Duration = Duration::from_secs(1);
/// With nothing from the controller for this long, everything held is let go (it sends `hb`
/// lines every second).
const QUIET: Duration = Duration::from_secs(3);

const PROMPT_OPEN: &str =
    "an ai-remote approval prompt is open on the host, so input is paused until the owner answers it";

#[derive(Debug, Deserialize, Default)]
struct Request {
    #[serde(default)]
    clipboard: bool,
}

/// One line from the controller.
#[derive(Debug, Deserialize, Default)]
struct Line {
    #[serde(default)]
    m: Option<[i64; 2]>,
    #[serde(default)]
    b: Option<String>,
    #[serde(default)]
    w: Option<[i64; 2]>,
    #[serde(default)]
    k: Option<String>,
    #[serde(default)]
    down: Option<bool>,
    #[serde(default)]
    t: Option<String>,
    #[serde(default)]
    clip: Option<String>,
    #[serde(default)]
    release: bool,
}

/// An event for the injection thread.
#[derive(Debug, Clone, PartialEq)]
enum Event {
    Move(i32, i32),
    Button(Button, bool),
    Wheel(i32, i32),
    Key(String, bool),
    Text(String),
    Release,
}

impl Event {
    /// Whether a backlog may drop this event: movement is replaced by the next one anyway.
    fn droppable(&self) -> bool {
        matches!(self, Event::Move(..) | Event::Wheel(..))
    }
}

/// Counts for the audit line at the end.
#[derive(Debug, Default)]
struct Counts {
    moves: AtomicUsize,
    buttons: AtomicUsize,
    keys: AtomicUsize,
    text_chars: AtomicUsize,
    clips_in: AtomicUsize,
    clips_out: AtomicUsize,
    errors: AtomicUsize,
}

pub struct InputProtocol {
    host: Arc<HostState>,
    streams: Arc<AtomicUsize>,
}

impl std::fmt::Debug for InputProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InputProtocol")
    }
}

impl InputProtocol {
    pub fn new(host: Arc<HostState>) -> Self {
        Self { host, streams: Arc::new(AtomicUsize::new(0)) }
    }
}

/// Counts a stream while it is served.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ProtocolHandler for InputProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let peer = match self.host.mesh().peer_for(&conn.remote_id()).await {
            Ok(Some(peer)) => peer,
            _ => {
                conn.close(CLOSE_NO_ACCESS.into(), REASON_NOT_PAIRED);
                return Ok(());
            }
        };
        if peer.level.is_none() {
            conn.close(CLOSE_NO_ACCESS.into(), REASON_NO_LEVEL);
            return Ok(());
        }
        while let Ok((mut send, recv)) = conn.accept_bi().await {
            if self.streams.fetch_add(1, Ordering::SeqCst) >= MAX_STREAMS {
                self.streams.fetch_sub(1, Ordering::SeqCst);
                reply(&mut send, json!({"ok": false, "error": "too many live input streams on this host"})).await;
                let _ = send.finish();
                continue;
            }
            let slot = Slot(self.streams.clone());
            let host = self.host.clone();
            let peer = peer.clone();
            tokio::spawn(async move {
                serve(host, peer, send, recv).await;
                drop(slot);
            });
        }
        Ok(())
    }
}

/// What the stream may do with the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClipGrant {
    read: bool,
    write: bool,
}

/// Why `peer` may not drive this host live now, if so: it must have taken over, have level
/// `control` (or the owner's bypass), and the policy must allow `mouse` and `keyboard`. Also
/// returns what it may do with the clipboard (`clipboard_read` / `clipboard_write` allowed).
async fn standing(host: &HostState, peer: &Peer, wants_clipboard: bool) -> Result<ClipGrant> {
    match host.turns().driver() {
        Driver::Person { device, .. } if device == peer.name => {}
        Driver::Person { device, .. } => {
            bail!("{device} has taken over this computer; live input is for whoever drives")
        }
        _ => bail!("take over first: live input is for the person who drives this computer"),
    }
    let Some(level) = host.effective_level(peer).await else {
        bail!("this device no longer has access to this host");
    };
    if level < Level::Control {
        bail!("live input requires level 'control', but this device has '{level}'");
    }
    if host.bypass().covers(peer) {
        return Ok(ClipGrant { read: wants_clipboard, write: wants_clipboard });
    }
    let decide = |name: &str| -> Option<Action> {
        let tool = host.registry().get(name)?;
        Some(policy::decide(&host.config().rules, name, peer, tool.spec().default_action))
    };
    for name in ["mouse", "keyboard"] {
        match decide(name) {
            None => bail!("the host has no '{name}' tool"),
            Some(Action::Allow) => {}
            Some(Action::Deny) => bail!("the host's policy does not allow '{name}'"),
            Some(Action::Ask) => bail!(
                "the host's policy asks the owner about every '{name}' action, which live input cannot do; \
                 actions are sent one at a time instead"
            ),
        }
    }
    // The clipboard is an extra: a policy that does not allow it turns only the sync off.
    let allowed = |name| wants_clipboard && decide(name) == Some(Action::Allow);
    Ok(ClipGrant { read: allowed("clipboard_read"), write: allowed("clipboard_write") })
}

/// `standing` plus what only matters when the stream opens: a desktop input can reach.
async fn admit(host: &HostState, peer: &Peer, wants_clipboard: bool) -> Result<ClipGrant> {
    let grant = standing(host, peer, wants_clipboard).await?;
    if let Some(why) = platform::desktop_blocker() {
        bail!("input cannot reach the host's desktop: {why}");
    }
    #[cfg(target_os = "macos")]
    if !platform::macos::accessibility_allowed() {
        bail!("macOS Accessibility permission is missing for this host; enable it and restart the host");
    }
    Ok(grant)
}

async fn reply(send: &mut SendStream, value: Value) -> bool {
    let mut line = value.to_string();
    line.push('\n');
    send.write_all(line.as_bytes()).await.is_ok()
}

/// One line read from the controller.
enum Read {
    Line,
    /// Longer than `MAX_LINE`: skipped up to its end.
    TooLong(usize),
    End,
}

async fn serve(host: Arc<HostState>, peer: Peer, mut send: SendStream, recv: RecvStream) {
    let mut reader = BufReader::new(recv);
    let mut first = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(15), read_line(&mut reader, &mut first)).await;
    let request: Request = match read {
        Ok(Ok(Read::Line)) => match serde_json::from_slice(&first) {
            Ok(r) => r,
            Err(e) => {
                reply(&mut send, json!({"ok": false, "error": format!("bad request: {e}")})).await;
                return;
            }
        },
        _ => return,
    };
    let bypass = host.bypass().covers(&peer).then_some(true);
    let audit = |kind: &str, extra: Value| {
        let mut v = json!({"kind": kind, "peer": peer.name, "principal": peer.principal});
        if let Some(b) = bypass {
            v["bypass"] = b.into();
        }
        if let (Some(a), Some(b)) = (v.as_object_mut(), extra.as_object()) {
            a.extend(b.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        host.audit().event(v);
    };
    let grant = match admit(&host, &peer, request.clipboard).await {
        Ok(grant) => grant,
        Err(e) => {
            let why = format!("{e:#}");
            audit("input_refused", json!({"error": why}));
            reply(&mut send, json!({"ok": false, "error": why})).await;
            let _ = send.finish();
            return;
        }
    };
    let os = match Os::CURRENT {
        Os::Windows => "windows",
        Os::Mac => "macos",
        Os::Other => "other",
    };
    if !reply(&mut send, json!({"ok": true, "os": os, "clipboard": grant.read, "clipboard_write": grant.write})).await {
        return;
    }
    audit("input_start", json!({"clipboard_read": grant.read, "clipboard_write": grant.write}));
    let counts = Arc::new(Counts::default());
    let started = Instant::now();

    // Lines for the controller, from the injection thread, the clipboard tasks and the reader.
    let (out, mut outbox) = mpsc::unbounded_channel::<Value>();
    // Events for the injection thread.
    let (events, inbox) = std_mpsc::channel::<Event>();
    let pending = Arc::new(AtomicUsize::new(0));
    let secure_attention = secure_attention_event(&host);
    let injector = {
        let (out, counts, pending, prompts, stopping) =
            (out.clone(), counts.clone(), pending.clone(), host.open_prompts(), host.stopping().clone());
        std::thread::Builder::new().name("ai-remote-live-input".into()).spawn(move || {
            let mut injector = match Injector::new(prompts, stopping, secure_attention) {
                Ok(i) => i,
                Err(e) => {
                    let _ = out.send(json!({"err": format!("{e:#}")}));
                    return;
                }
            };
            injector.run(inbox, &pending, &out, &counts);
        })
    };
    if let Err(e) = injector {
        reply(&mut send, json!({"err": format!("cannot start the input thread: {e}")})).await;
        audit("input_end", json!({"error": e.to_string()}));
        return;
    }

    // The clipboard: what may be read and written now (the standing check updates both), what the
    // controller last set (not reported back as a change), and writes done one after another.
    let may_read = Arc::new(AtomicBool::new(grant.read));
    let may_write = Arc::new(AtomicBool::new(grant.write));
    let clip = Arc::new(std::sync::Mutex::new(ClipState::default()));
    let poller = request.clipboard.then(|| {
        let (out, clip, counts, stopping, may_read) =
            (out.clone(), clip.clone(), counts.clone(), host.stopping().clone(), may_read.clone());
        tokio::spawn(async move { poll_clipboard(out, clip, counts, stopping, may_read).await })
    });
    let (clip_writes, mut clip_queue) = mpsc::unbounded_channel::<String>();
    let writer = {
        let (out, clip) = (out.clone(), clip.clone());
        tokio::spawn(async move {
            while let Some(text) = clip_queue.recv().await {
                let written = text.clone();
                let result = tokio::task::spawn_blocking(move || write_clipboard(&written)).await;
                let mut state = clip.lock().unwrap();
                state.writing -= 1;
                match result {
                    Ok(Ok(())) => state.set_by_controller(&text),
                    Ok(Err(e)) => drop(out.send(json!({"err": format!("{e:#}")}))),
                    Err(e) => drop(out.send(json!({"err": format!("clipboard task failed: {e}")}))),
                }
            }
        })
    };

    let ended = std::sync::Mutex::new(None::<String>);
    let reader_task = {
        let (events, pending, out, clip, counts, stopping) =
            (events, pending.clone(), out.clone(), clip.clone(), counts.clone(), host.stopping().clone());
        let (host, peer, ended, may_read, may_write) = (&host, &peer, &ended, may_read.clone(), may_write.clone());
        async move {
            let mut line = Vec::new();
            let mut checks = tokio::time::interval(STANDING);
            checks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last_line = Instant::now();
            let mut quiet_released = false;
            loop {
                let got = tokio::select! {
                    biased;
                    _ = stopping.cancelled() => break,
                    _ = checks.tick() => {
                        match standing(host, peer, request.clipboard).await {
                            Ok(now) => {
                                may_read.store(now.read, Ordering::Relaxed);
                                may_write.store(now.write, Ordering::Relaxed);
                            }
                            Err(e) => {
                                let why = format!("live input ended: {e:#}");
                                let _ = out.send(json!({"err": why}));
                                *ended.lock().unwrap() = Some(why);
                                break;
                            }
                        }
                        if !quiet_released && last_line.elapsed() >= QUIET {
                            quiet_released = true;
                            pending.fetch_add(1, Ordering::Relaxed);
                            let _ = events.send(Event::Release);
                        }
                        continue;
                    }
                    got = read_line(&mut reader, &mut line) => got,
                };
                last_line = Instant::now();
                quiet_released = false;
                match got {
                    Ok(Read::Line) => {}
                    Ok(Read::TooLong(n)) => {
                        counts.errors.fetch_add(1, Ordering::Relaxed);
                        let _ = out.send(json!({"err": format!("a line of {n} bytes was too long and was skipped")}));
                        continue;
                    }
                    Ok(Read::End) => break,
                    Err(e) => {
                        let _ = out.send(json!({"err": format!("{e}")}));
                        break;
                    }
                }
                let parsed: Line = match serde_json::from_slice(&line) {
                    Ok(l) => l,
                    Err(e) => {
                        counts.errors.fetch_add(1, Ordering::Relaxed);
                        let _ = out.send(json!({"err": format!("bad line: {e}")}));
                        continue;
                    }
                };
                if let Some(text) = parsed.clip {
                    let refusal = if !may_write.load(Ordering::Relaxed) {
                        Some("the host's policy does not allow 'clipboard_write' on this stream".to_string())
                    } else if text.len() > MAX_CLIP {
                        Some(format!("clipboard text is too long ({} bytes)", text.len()))
                    } else {
                        let session = platform::session_state();
                        (!session.desktop_usable || session.secure_desktop).then(|| {
                            "the host's desktop is locked or not usable, so its clipboard is left alone".to_string()
                        })
                    };
                    if let Some(why) = refusal {
                        counts.errors.fetch_add(1, Ordering::Relaxed);
                        let _ = out.send(json!({"err": why}));
                        continue;
                    }
                    counts.clips_in.fetch_add(1, Ordering::Relaxed);
                    clip.lock().unwrap().writing += 1;
                    let _ = clip_writes.send(text);
                    continue;
                }
                let event = match event_of(parsed) {
                    Ok(Some(e)) => e,
                    Ok(None) => continue,
                    Err(e) => {
                        counts.errors.fetch_add(1, Ordering::Relaxed);
                        let _ = out.send(json!({"err": format!("{e}")}));
                        continue;
                    }
                };
                if event.droppable() && pending.load(Ordering::Relaxed) >= MAX_PENDING {
                    continue;
                }
                pending.fetch_add(1, Ordering::Relaxed);
                if events.send(event).is_err() {
                    break;
                }
            }
            // Dropping `events` ends the injection thread, which releases what it holds.
        }
    };
    let writer_task = async {
        while let Some(value) = outbox.recv().await {
            if !reply(&mut send, value).await {
                break;
            }
        }
        let _ = send.finish();
    };
    tokio::pin!(writer_task);
    tokio::select! {
        _ = reader_task => {}
        _ = &mut writer_task => {}
    }
    if let Some(poller) = poller {
        poller.abort();
    }
    writer.abort();
    drop(out);
    // The reason the stream ended reaches the controller before the stream closes.
    let _ = tokio::time::timeout(Duration::from_millis(500), writer_task).await;
    let mut end = json!({
        "seconds": started.elapsed().as_secs_f64(),
        "moves": counts.moves.load(Ordering::Relaxed),
        "buttons": counts.buttons.load(Ordering::Relaxed),
        "keys": counts.keys.load(Ordering::Relaxed),
        "text_chars": counts.text_chars.load(Ordering::Relaxed),
        "clips_in": counts.clips_in.load(Ordering::Relaxed),
        "clips_out": counts.clips_out.load(Ordering::Relaxed),
        "errors": counts.errors.load(Ordering::Relaxed),
    });
    if let Some(why) = ended.lock().unwrap().take() {
        end["reason"] = why.into();
    }
    audit("input_end", end);
}

/// Reads one line (without the newline) into `buf`. A line longer than `MAX_LINE` is read to its
/// end and dropped, so one oversized line costs only itself.
async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R, buf: &mut Vec<u8>) -> Result<Read> {
    buf.clear();
    let mut skipped = 0usize;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if buf.is_empty() && skipped == 0 { Read::End } else { Read::Line });
        }
        let (chunk, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (&available[..i], Some(i + 1)),
            None => (available, None),
        };
        let taken = chunk.len();
        if skipped > 0 || buf.len() + taken > MAX_LINE {
            skipped += buf.len() + taken;
            buf.clear();
        } else {
            buf.extend_from_slice(chunk);
        }
        reader.consume(done.unwrap_or(taken));
        if done.is_some() {
            return Ok(if skipped > 0 { Read::TooLong(skipped) } else { Read::Line });
        }
    }
}

/// Turns a parsed line into an event, validating sizes; `None` for a line that carries none.
fn event_of(line: Line) -> Result<Option<Event>> {
    if line.release {
        return Ok(Some(Event::Release));
    }
    if let Some([x, y]) = line.m {
        let coord = |v: i64| i32::try_from(v).map_err(|_| anyhow!("pointer coordinate {v} is out of range"));
        return Ok(Some(Event::Move(coord(x)?, coord(y)?)));
    }
    if let Some(b) = line.b {
        let button = match b.as_str() {
            "left" => Button::Left,
            "right" => Button::Right,
            "middle" => Button::Middle,
            other => bail!("unknown button '{other}'"),
        };
        let Some(down) = line.down else { bail!("a button event needs down: true|false") };
        return Ok(Some(Event::Button(button, down)));
    }
    if let Some([h, v]) = line.w {
        if h.abs() > MAX_SCROLL || v.abs() > MAX_SCROLL {
            bail!("wheel notches must be within ±{MAX_SCROLL}");
        }
        return Ok(Some(Event::Wheel(h as i32, v as i32)));
    }
    if let Some(k) = line.k {
        let Some(down) = line.down else { bail!("a key event needs down: true|false") };
        return Ok(Some(Event::Key(k, down)));
    }
    if let Some(t) = line.t {
        if t.chars().count() > MAX_TEXT {
            bail!("text is longer than {MAX_TEXT} characters");
        }
        if t.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
            bail!("text must not contain control characters");
        }
        return Ok(Some(Event::Text(t)));
    }
    Ok(None)
}

fn secure_attention_event(host: &HostState) -> Option<String> {
    #[cfg(windows)]
    {
        let profile = host.profile();
        crate::service::secure_attention_available(profile).then(|| crate::service::sas_event_name(profile))
    }
    #[cfg(not(windows))]
    {
        let _ = host;
        None
    }
}

/// The thread that sends input: one backend (macOS keeps multi-click timing in it), the keys and
/// buttons it holds, and the checks of the `mouse` / `keyboard` tools before every press.
struct Injector {
    backend: EnigoBackend,
    prompts: OpenPrompts,
    stopping: tokio_util::sync::CancellationToken,
    secure_attention: Option<String>,
    held_keys: Vec<(String, Key)>,
    held_buttons: Vec<Button>,
    /// Modifiers the stream holds, for the refused-chord check.
    modifiers: Vec<Modifier>,
    last_recheck: Instant,
    /// Why input is paused, as last told to the controller.
    blocked: Option<String>,
}

impl Injector {
    fn new(
        prompts: OpenPrompts,
        stopping: tokio_util::sync::CancellationToken,
        secure_attention: Option<String>,
    ) -> Result<Self> {
        Ok(Self {
            backend: EnigoBackend::new()?,
            prompts,
            stopping,
            secure_attention,
            held_keys: Vec::new(),
            held_buttons: Vec::new(),
            modifiers: Vec::new(),
            last_recheck: Instant::now(),
            blocked: None,
        })
    }

    fn run(
        &mut self,
        inbox: std_mpsc::Receiver<Event>,
        pending: &AtomicUsize,
        out: &mpsc::UnboundedSender<Value>,
        counts: &Counts,
    ) {
        while let Ok(first) = inbox.recv() {
            // Everything that arrived meanwhile, with runs of moves collapsed to the last one: a
            // slow desktop then lands on where the pointer is, not where it was.
            let mut batch = vec![first];
            // Every event taken off the channel, merged moves included: `pending` counted each.
            let mut n = 1;
            while let Ok(next) = inbox.try_recv() {
                n += 1;
                if let (Some(Event::Move(..)), Event::Move(..)) = (batch.last(), &next) {
                    batch.pop();
                }
                batch.push(next);
            }
            for event in batch {
                if self.stopping.is_cancelled() {
                    self.release_all();
                    return;
                }
                match self.apply(event, counts) {
                    Ok(()) => {}
                    Err(e) => {
                        counts.errors.fetch_add(1, Ordering::Relaxed);
                        let _ = out.send(json!({"err": format!("{e:#}")}));
                    }
                }
                self.report_blocked(out);
            }
            let _ = pending.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |p| Some(p.saturating_sub(n)));
        }
        self.release_all();
    }

    /// Tells the controller when input becomes paused or resumes.
    fn report_blocked(&mut self, out: &mpsc::UnboundedSender<Value>) {
        let now = self.blocker();
        if now != self.blocked {
            // Nothing stays pressed while nobody may press anything (a prompt, a locked desktop).
            if now.is_some() {
                self.release_all();
            }
            let _ = out.send(json!({"blocked": now}));
            self.blocked = now;
        }
    }

    /// Why nothing may be sent right now, if so.
    fn blocker(&self) -> Option<String> {
        if self.prompts.count() > 0 {
            return Some(PROMPT_OPEN.to_string());
        }
        let state = platform::session_state();
        if !state.desktop_usable {
            return Some(format!(
                "input cannot reach the host's desktop: {}",
                state.problem.as_deref().unwrap_or("the desktop is not usable")
            ));
        }
        None
    }

    /// The check before a press, a wheel notch or text: like the tools' before every event.
    fn check(&mut self, target: InputTarget) -> Result<()> {
        if let Some(why) = self.blocker() {
            bail!(why);
        }
        #[cfg(windows)]
        platform::follow_input_desktop();
        self.last_recheck = Instant::now();
        match platform::check_input_target(target) {
            InputTargetCheck::Allowed | InputTargetCheck::Doubtful(_) => Ok(()),
            InputTargetCheck::Refused(reason) => Err(anyhow!(reason)),
        }
    }

    fn apply(&mut self, event: Event, counts: &Counts) -> Result<()> {
        match event {
            Event::Move(x, y) => {
                if self.last_recheck.elapsed() > RECHECK {
                    self.last_recheck = Instant::now();
                    self.blocked = self.blocker();
                    #[cfg(windows)]
                    if self.blocked.is_none() {
                        platform::follow_input_desktop();
                    }
                }
                // While input is paused moves are dropped quietly: `blocked` already said why.
                if self.blocked.is_some() {
                    return Ok(());
                }
                counts.moves.fetch_add(1, Ordering::Relaxed);
                self.backend.move_to(x, y)
            }
            Event::Button(button, true) => {
                self.check(InputTarget::Pointer)?;
                counts.buttons.fetch_add(1, Ordering::Relaxed);
                self.held_buttons.push(button);
                self.backend.button(button, Direction::Press)
            }
            Event::Button(button, false) => {
                if !self.held_buttons.contains(&button) {
                    return Ok(());
                }
                #[cfg(windows)]
                platform::follow_input_desktop();
                self.backend.button(button, Direction::Release)?;
                forget(&mut self.held_buttons, &button);
                Ok(())
            }
            Event::Wheel(h, v) => {
                self.check(InputTarget::Pointer)?;
                if v != 0 {
                    self.backend.scroll(v, Axis::Vertical)?;
                }
                if h != 0 {
                    self.backend.scroll(h, Axis::Horizontal)?;
                }
                Ok(())
            }
            Event::Key(name, true) => self.press_key(&name, counts),
            Event::Key(name, false) => self.release_key(&name),
            Event::Text(text) => {
                self.check(InputTarget::Focused)?;
                counts.text_chars.fetch_add(text.chars().count(), Ordering::Relaxed);
                self.backend.text(&text)
            }
            Event::Release => {
                self.release_all();
                Ok(())
            }
        }
    }

    fn press_key(&mut self, name: &str, counts: &Counts) -> Result<()> {
        // A press of a key already down is the controller's auto-repeat: press it again (the OS
        // repeats it like a held key would), without holding it twice.
        if let Some(&(_, key)) = self.held_keys.iter().find(|(held, _)| held == name) {
            self.check(InputTarget::Focused)?;
            counts.keys.fetch_add(1, Ordering::Relaxed);
            return self.backend.key(key, Direction::Press);
        }
        let parsed = keys::parse_key_name(name)?;
        let resolved = match parsed {
            KeyName::Modifier(m) => Ok((m.key(), Vec::new())),
            other => keys::resolve(other),
        };
        let (key, layout) = match resolved {
            Ok(r) => r,
            // A character from the controller's layout that no key here types (é on a US layout):
            // enter it as text, as typing it would, unless it is part of a shortcut.
            Err(e) => {
                let shortcut =
                    self.modifiers.iter().any(|m| matches!(m, Modifier::Ctrl | Modifier::Alt | Modifier::Meta));
                let KeyName::Char(c) = parsed else { return Err(e) };
                if shortcut {
                    return Err(e);
                }
                self.check(InputTarget::Focused)?;
                counts.text_chars.fetch_add(1, Ordering::Relaxed);
                let shift = self.modifiers.contains(&Modifier::Shift);
                let typed: String = if shift { c.to_uppercase().collect() } else { c.to_string() };
                return self.backend.text(&typed);
            }
        };
        let mut modifiers = self.modifiers.clone();
        modifiers.extend(layout.iter().copied());
        modifiers.sort();
        modifiers.dedup();
        let pressed = Pressed { key, modifiers };
        if keys::is_secure_attention(&pressed, Os::CURRENT) {
            let Some(event) = self.secure_attention.clone() else {
                bail!(
                    "ctrl+alt+del is refused: it opens the secure attention screen (a host installed as a service can send it)"
                );
            };
            self.check(InputTarget::Focused)?;
            // The tool path never presses the keys for it; let go of the ones held for the chord.
            self.release_all();
            return self.backend.secure_attention(&event);
        }
        if let Some(why) = keys::forbidden(&pressed, Os::CURRENT) {
            bail!("'{name}' with the held modifiers is refused: {why}");
        }
        self.check(InputTarget::Focused)?;
        counts.keys.fetch_add(1, Ordering::Relaxed);
        // Modifiers the host's layout needs for this character (shift for '+' on a US layout) are
        // held around the press only: the controller holds its own.
        let transient: Vec<Modifier> = layout.into_iter().filter(|m| !self.modifiers.contains(m)).collect();
        for m in &transient {
            self.backend.key(m.key(), Direction::Press)?;
        }
        if let KeyName::Modifier(m) = parsed
            && !self.modifiers.contains(&m)
        {
            self.modifiers.push(m);
        }
        self.held_keys.push((name.to_string(), key));
        let result = self.backend.key(key, Direction::Press);
        for m in transient.iter().rev() {
            let _ = self.backend.key(m.key(), Direction::Release);
        }
        result
    }

    /// Lets go of a key this stream pressed; a key it does not hold (typed as text, refused, or
    /// pressed before a release-all) has nothing to release.
    fn release_key(&mut self, name: &str) -> Result<()> {
        let Some(i) = self.held_keys.iter().rposition(|(held, _)| held == name) else { return Ok(()) };
        let key = self.held_keys[i].1;
        #[cfg(windows)]
        platform::follow_input_desktop();
        // Forgotten only once it is really up: a failed release is tried again when the stream ends.
        self.backend.key(key, Direction::Release)?;
        self.held_keys.remove(i);
        if let Ok(KeyName::Modifier(m)) = keys::parse_key_name(name) {
            self.modifiers.retain(|held| *held != m);
        }
        Ok(())
    }

    /// Buttons first, then keys, each in reverse press order: the way a person lets go. On the
    /// desktop that has the input now (the one they were pressed on may be gone).
    fn release_all(&mut self) {
        if self.held_buttons.is_empty() && self.held_keys.is_empty() {
            self.modifiers.clear();
            return;
        }
        #[cfg(windows)]
        platform::follow_input_desktop();
        for b in self.held_buttons.drain(..).rev() {
            if let Err(e) = self.backend.button(b, Direction::Release) {
                tracing::warn!(?b, %e, "releasing a mouse button failed");
            }
        }
        for (_, k) in self.held_keys.drain(..).rev() {
            if let Err(e) = self.backend.key(k, Direction::Release) {
                tracing::warn!(?k, %e, "releasing a key failed");
            }
        }
        self.modifiers.clear();
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        self.release_all();
    }
}

fn forget<T: PartialEq>(held: &mut Vec<T>, item: &T) {
    if let Some(i) = held.iter().rposition(|h| h == item) {
        held.remove(i);
    }
}

/// What the clipboard poller last saw, and what the controller last set (not reported back).
#[derive(Debug, Default)]
struct ClipState {
    /// The first read only records what the clipboard holds; changes after it are reported.
    primed: bool,
    last_text: Option<String>,
    last_files: Vec<String>,
    /// What the controller last set, so the poller does not report it back. Cleared once the
    /// clipboard holds something else, so the same text copied later on the host is reported.
    from_controller: Option<String>,
    /// Controller writes queued or under way: the poller waits for them, or it could read the
    /// old content and report it as new.
    writing: usize,
}

impl ClipState {
    fn set_by_controller(&mut self, text: &str) {
        self.from_controller = Some(text.to_string());
        self.last_text = Some(text.to_string());
    }
}

/// What the host's clipboard holds right now.
#[derive(Debug, Default, PartialEq)]
struct ClipContent {
    text: Option<String>,
    files: Vec<String>,
}

fn read_clipboard() -> Result<ClipContent> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| anyhow!("clipboard: {e}"))?;
    let files = clipboard.get().file_list().unwrap_or_default();
    let text = match clipboard.get_text() {
        Ok(text) => Some(text),
        Err(arboard::Error::ContentNotAvailable) => None,
        Err(arboard::Error::ClipboardOccupied) => bail!("busy"),
        Err(e) => bail!("clipboard: {e}"),
    };
    Ok(ClipContent { text, files: files.into_iter().map(|p| p.to_string_lossy().into_owned()).collect() })
}

fn write_clipboard(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| anyhow!("clipboard: {e}"))?;
    clipboard.set_text(text).map_err(|e| match e {
        arboard::Error::ClipboardOccupied => anyhow!("another application is holding the clipboard; try again"),
        other => anyhow!("clipboard: {other}"),
    })
}

/// The clipboard's change counter on Windows, readable without opening the clipboard: polling by
/// opening it every few hundred milliseconds would make the user's own copy and paste fail now and
/// then ("clipboard is in use"). `None` elsewhere (reading the macOS pasteboard takes no lock).
fn clipboard_sequence() -> Option<u32> {
    #[cfg(windows)]
    {
        // SAFETY: a plain query with no arguments.
        Some(unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() })
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Reports the host's clipboard text and file list when they change.
async fn poll_clipboard(
    out: mpsc::UnboundedSender<Value>,
    clip: Arc<std::sync::Mutex<ClipState>>,
    counts: Arc<Counts>,
    stopping: tokio_util::sync::CancellationToken,
    may_read: Arc<AtomicBool>,
) {
    let mut ticker = tokio::time::interval(CLIP_POLL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_sequence = None;
    loop {
        tokio::select! {
            _ = stopping.cancelled() => return,
            _ = ticker.tick() => {}
        }
        let session = platform::session_state();
        if !session.desktop_usable || session.secure_desktop || !may_read.load(Ordering::Relaxed) {
            continue;
        }
        // A controller write in flight: reading now could report the old content as new.
        if clip.lock().unwrap().writing > 0 {
            continue;
        }
        let sequence = clipboard_sequence();
        if sequence.is_some() && sequence == last_sequence {
            continue;
        }
        let Ok(Ok(content)) = tokio::task::spawn_blocking(read_clipboard).await else { continue };
        let mut messages = Vec::new();
        {
            let mut state = clip.lock().unwrap();
            // A write started while this read ran: its result is not known yet, read again later.
            if state.writing > 0 {
                continue;
            }
            last_sequence = sequence;
            if !state.primed {
                state.primed = true;
                state.last_files = content.files;
                if content.text.is_some() {
                    state.last_text = content.text;
                }
                continue;
            }
            if content.files != state.last_files {
                // An empty list too: the controller then stops offering the files that are gone.
                state.last_files = content.files.clone();
                messages.push(json!({"clip_files": content.files}));
            }
            if let Some(text) = content.text.filter(|t| t.len() <= MAX_CLIP)
                && state.last_text.as_deref() != Some(text.as_str())
            {
                let echo = state.from_controller.as_deref() == Some(text.as_str());
                if !echo {
                    // The clipboard moved on from what the controller set.
                    state.from_controller = None;
                }
                state.last_text = Some(text.clone());
                if !echo && content.files.is_empty() {
                    messages.push(json!({"clip": text}));
                }
            }
        }
        for m in messages {
            counts.clips_out.fetch_add(1, Ordering::Relaxed);
            if out.send(m).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Option<Event>> {
        event_of(serde_json::from_str(s).unwrap())
    }

    #[test]
    fn lines_become_events() {
        assert_eq!(parse(r#"{"m":[10,20]}"#).unwrap(), Some(Event::Move(10, 20)));
        assert_eq!(parse(r#"{"b":"left","down":true}"#).unwrap(), Some(Event::Button(Button::Left, true)));
        assert_eq!(parse(r#"{"w":[0,-3]}"#).unwrap(), Some(Event::Wheel(0, -3)));
        assert_eq!(parse(r#"{"k":"ctrl","down":false}"#).unwrap(), Some(Event::Key("ctrl".into(), false)));
        assert_eq!(parse(r#"{"t":"héllo"}"#).unwrap(), Some(Event::Text("héllo".into())));
        assert_eq!(parse(r#"{"release":true}"#).unwrap(), Some(Event::Release));
        assert_eq!(parse(r#"{"ping":1}"#).unwrap(), None);
    }

    #[test]
    fn bad_lines_are_refused() {
        assert!(parse(r#"{"b":"side","down":true}"#).is_err());
        assert!(parse(r#"{"b":"left"}"#).is_err());
        assert!(parse(r#"{"w":[0,900]}"#).is_err());
        assert!(parse(r#"{"m":[1,99999999999]}"#).is_err());
        assert!(parse("{\"t\":\"a\\u0007b\"}").is_err());
        assert!(parse(&format!(r#"{{"t":"{}"}}"#, "x".repeat(MAX_TEXT + 1))).is_err());
    }

    #[tokio::test]
    async fn an_overlong_line_is_skipped_and_the_next_one_read() {
        let mut data = vec![b'x'; MAX_LINE + 10];
        data.extend_from_slice(b"\n{\"m\":[1,2]}\n");
        let mut reader = BufReader::new(&data[..]);
        let mut buf = Vec::new();
        assert!(matches!(read_line(&mut reader, &mut buf).await.unwrap(), Read::TooLong(n) if n == MAX_LINE + 10));
        assert!(matches!(read_line(&mut reader, &mut buf).await.unwrap(), Read::Line));
        assert_eq!(buf, br#"{"m":[1,2]}"#);
        assert!(matches!(read_line(&mut reader, &mut buf).await.unwrap(), Read::End));
    }

    #[test]
    fn only_movement_is_droppable() {
        assert!(Event::Move(1, 1).droppable());
        assert!(Event::Wheel(0, 1).droppable());
        assert!(!Event::Button(Button::Left, false).droppable());
        assert!(!Event::Key("a".into(), false).droppable());
        assert!(!Event::Release.droppable());
    }

    #[test]
    fn the_controllers_clipboard_text_is_not_echoed() {
        let mut state = ClipState::default();
        state.set_by_controller("hello");
        assert_eq!(state.from_controller.as_deref(), Some("hello"));
        assert_eq!(state.last_text.as_deref(), Some("hello"));
    }
}
