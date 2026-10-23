//! Codex, driven through `codex app-server` (JSON-RPC over stdio): `initialize`, then a thread
//! (`thread/start`, or `thread/resume` to continue one), then one `turn/start` per message on the
//! same process. Measured with codex-cli 0.163: two turns on one thread keep their context,
//! `turn/interrupt` ends a running turn within milliseconds (`turn/completed` with status
//! `interrupted`), and `thread/resume` in a new process continues the conversation.
//!
//! - Bypass: the thread starts with `approvalPolicy: "never"` and `sandbox: "danger-full-access"`;
//!   approval requests that still come are accepted.
//! - `item/tool/requestUserInput` (the agent asks the user) becomes question events carrying the
//!   request they belong to; the answers go back when every question of the request has one (an
//!   empty answer skips it and is left out of the reply), and a `question_done` event says how the
//!   request ended: answered, skipped, resolved by Codex (`serverRequest/resolved`, or the turn
//!   ended) or dismissed (the turn was stopped). Several requests can wait at once. Codex only lets
//!   the model ask outside plan mode with `features.default_mode_request_user_input`, which the
//!   adapter turns on (0.137 and 0.150 have it; a Codex that does not know a feature ignores it).
//!   Such a question does not block the turn (`isBlocking: false`): Codex's own clients answer it
//!   with nothing after a while so the model decides by itself, and so does the adapter, after
//!   [`QUESTION_WAIT`] (the question says when, `expires_in_ms`; a command-line run cannot answer
//!   at all). A request is named in events by this process's start and its JSON-RPC id, since
//!   Codex numbers requests from 0 in every process and a session outlives its processes.
//! - Reasoning: only its summary is shown (as Codex's own app does), one paragraph per summary
//!   part; the raw reasoning text is left out.
//! - A model the account cannot use fails every turn with an `error` notification before
//!   `turn/completed`; its message becomes the turn's reason, so the user sees why and can pick
//!   another model.
//! - Notifications of other threads (an app-server may host several) are ignored.
//! - Steer: `turn/steer` with the running turn's id (`expectedTurnId`) adds a message to it; the
//!   agent reads it at its next step (measured with 0.163: answered in 7 ms, a count to 20 stopped
//!   at 5 when told to). It fails with `no active turn to steer` once the turn is over; the message
//!   then goes to the queue. A steer sent before `turn/start` has answered waits for the turn id.
//! - Command output streams as `item/commandExecution/outputDelta` (tool updates carrying one
//!   piece of text each, up to [`PREVIEW`] per command); file changes carry unified diffs, shown
//!   one diff per hunk; `thread/tokenUsage/updated` gives the context window and how much of it
//!   the last request used.
//! - Skills: `skills/list` once the thread is up; they go to the `/` menu as `$name`, which is how
//!   Codex names a skill in a message, and a message naming one carries it as a `skill` input
//!   item (its name and path) beside the text, as Codex's own clients send it.
//! - Commands outlive an interrupt: Codex runs shell commands as "background terminals" (unified
//!   exec), and `turn/interrupt` only cancels the turn, so a running `sleep 45` keeps going (seen
//!   on Windows with 0.162; the same code path on every platform). Once a turn ends interrupted,
//!   the adapter asks for `thread/backgroundTerminals/list` and terminates the ones whose item
//!   was started in that turn; terminals of earlier turns (a dev server the user asked for) stay.
//!   Both methods are experimental (0.150 has them, 0.137 does not): `initialize` opts in with
//!   `experimentalApi`, and a Codex without them leaves its commands running as before.

use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{LiveCmd, exit_reason, report};
use crate::chat::{
    Image,
    event::{
        DIFF_SIDE, DIFF_TOTAL, Ev, Out, PREVIEW, PlanEntry, SlashCommand, TURN_DIFF, ToolContent, ToolKind, ToolStatus,
        TurnStatus, cut, preview_value,
    },
    jsonrpc::{self, Incoming},
    proc::{AgentProc, Line},
};

pub struct Opts {
    pub cwd: String,
    pub model: Option<String>,
    pub resume: Option<String>,
}

pub fn args(_l: &super::Launch<'_>) -> Vec<String> {
    vec!["-c".into(), "features.default_mode_request_user_input=true".into(), "app-server".into()]
}

/// How long a question that does not block the turn waits for an answer before the agent goes on
/// without one (Codex's own clients wait about as long).
pub const QUESTION_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// A question request waiting for answers.
#[derive(Debug)]
struct Asking {
    /// Its JSON-RPC id, and the name events give it.
    id: Value,
    request: String,
    qids: Vec<String>,
    answers: HashMap<String, String>,
    /// When it is answered with nothing (a question that does not block the turn).
    deadline: Option<tokio::time::Instant>,
}

/// How a request is named in events: this process's start and the JSON-RPC id (Codex numbers its
/// requests from 0 in each process).
fn request_name(st: &State, id: &Value) -> String {
    let id = match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    format!("{}-{id}", st.epoch)
}

/// What a request we sent was for.
#[derive(Debug)]
enum Pending {
    Initialize,
    Thread {
        resumed: bool,
    },
    TurnStart,
    Steer {
        id: String,
    },
    Skills,
    /// The background terminals, listed to stop those started by these command items.
    Terminals {
        items: HashSet<String>,
    },
    Other,
}

#[derive(Default)]
struct State {
    next: u64,
    pending: HashMap<u64, Pending>,
    thread: Option<String>,
    turn: Option<String>,
    ready: bool,
    waiting: VecDeque<(String, Vec<Image>)>,
    streamed: HashSet<String>,
    final_text: Option<String>,
    last_error: Option<String>,
    /// Question requests waiting for answers, oldest first.
    asking: Vec<Asking>,
    /// When this process started (ms since the epoch), which names its question requests.
    epoch: u128,
    /// A turn was asked for and has no id yet.
    starting: bool,
    /// Steers waiting for that id.
    steer_wait: Vec<(String, String, Vec<Image>)>,
    /// Bytes of output streamed per command.
    output: HashMap<String, usize>,
    /// File changes whose diff was shown.
    diffed: HashSet<String>,
    /// The skills Codex offers: name and path.
    skills: Vec<(String, String)>,
    /// Command items started in the current turn: stopped if it ends interrupted.
    commands: HashSet<String>,
    /// Where each agent message belongs (`commentary` or `final_answer`), as its item started.
    phases: HashMap<String, String>,
    /// The running turn's whole diff as Codex last reported it (`turn/diff/updated`); sent as the turn ends.
    turn_diff: Option<String>,
}

/// The input items of a message: its text, every skill it names as `$name`, and its images (as
/// data URLs).
fn input_items(st: &State, text: &str, images: &[Image]) -> Value {
    let mut items = vec![json!({"type": "text", "text": text, "text_elements": []})];
    for i in images {
        items.push(json!({"type": "image", "url": format!("data:{};base64,{}", i.mime, i.data)}));
    }
    for (name, path) in &st.skills {
        if names_skill(text, name) {
            items.push(json!({"type": "skill", "name": name, "path": path}));
        }
    }
    Value::Array(items)
}

/// Whether `text` has `$name` as a word of its own.
fn names_skill(text: &str, name: &str) -> bool {
    let token = format!("${name}");
    text.match_indices(&token).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + token.len()..].chars().next();
        before.is_none_or(char::is_whitespace)
            && after.is_none_or(|c| !(c.is_alphanumeric() || c == '-' || c == '_' || c == ':'))
    })
}

impl State {
    fn request(&mut self, method: &str, params: Value, what: Pending) -> String {
        self.next += 1;
        self.pending.insert(self.next, what);
        jsonrpc::request(self.next, method, params)
    }
}

pub async fn run(
    proc: AgentProc,
    mut lines: mpsc::Receiver<Line>,
    mut cmds: mpsc::Receiver<LiveCmd>,
    out: mpsc::Sender<Out>,
    name: String,
    opts: Opts,
) {
    let mut st = State {
        epoch: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0),
        ..Default::default()
    };
    let init = st.request(
        "initialize",
        json!({"clientInfo": {"name": "ai-remote", "title": "ai-remote", "version": ar_core::VERSION}, "capabilities": {"experimentalApi": true}}),
        Pending::Initialize,
    );
    if proc.write_line(init).await.is_err() {
        report(&out, Out::Exited(exit_reason(&name, &proc, Some("could not start".into())))).await;
        return;
    }
    loop {
        tokio::select! {
            _ = next_deadline(&st) => {
                for (reply, done) in expire(&mut st, tokio::time::Instant::now()) {
                    let _ = proc.write_line(reply).await;
                    report(&out, Out::Ev(done)).await;
                }
            }
            line = lines.recv() => match line {
                Some(Ok(line)) => {
                    let (writes, outs) = handle(&mut st, &line, &opts);
                    for w in writes {
                        let _ = proc.write_line(w).await;
                    }
                    for o in outs {
                        if !report(&out, o).await {
                            proc.shutdown(std::time::Duration::from_secs(2)).await;
                            return;
                        }
                    }
                }
                Some(Err(why)) => {
                    let reason = exit_reason(&name, &proc, Some(why));
                    proc.shutdown(std::time::Duration::from_secs(1)).await;
                    report(&out, Out::Exited(reason)).await;
                    return;
                }
                None => {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    let reason = exit_reason(&name, &proc, None);
                    proc.shutdown(std::time::Duration::from_secs(1)).await;
                    report(&out, Out::Exited(reason)).await;
                    return;
                }
            },
            cmd = cmds.recv() => match cmd {
                Some(LiveCmd::Prompt { text, images }) => {
                    if st.ready {
                        let line = turn_start(&mut st, &text, &images);
                        let _ = proc.write_line(line).await;
                    } else {
                        st.starting = true;
                        st.waiting.push_back((text, images));
                    }
                }
                Some(LiveCmd::Steer { id, text, images }) => match steer(&mut st, id, text, images) {
                    Ok(Some(line)) => {
                        let _ = proc.write_line(line).await;
                    }
                    Ok(None) => {}
                    Err(o) => {
                        report(&out, *o).await;
                    }
                },
                Some(LiveCmd::Interrupt) => match (st.thread.clone(), st.turn.clone()) {
                    (Some(thread), Some(turn)) => {
                        let line = st.request("turn/interrupt", json!({"threadId": thread, "turnId": turn}), Pending::Other);
                        let _ = proc.write_line(line).await;
                    }
                    _ => {
                        // Nothing started yet: drop what waits and end the turn here.
                        st.waiting.clear();
                        report(&out, Out::TurnEnd { status: TurnStatus::Interrupted, final_text: None, reason: None }).await;
                    }
                },
                Some(LiveCmd::Answer { qid, text }) => {
                    if let Some((reply, done)) = answer(&mut st, qid, text) {
                        let _ = proc.write_line(reply).await;
                        report(&out, Out::Ev(done)).await;
                    }
                }
                Some(LiveCmd::Shutdown { done }) => {
                    proc.shutdown(std::time::Duration::from_secs(3)).await;
                    let _ = done.send(());
                    return;
                }
                None => {
                    proc.shutdown(std::time::Duration::from_secs(3)).await;
                    return;
                }
            },
        }
    }
}

/// Records the answer to question `qid` (of the latest request still waiting for it); once every
/// question of that request has one, the reply to write and the event saying it is over.
fn answer(st: &mut State, qid: String, text: String) -> Option<(String, Ev)> {
    let i = st.asking.iter().rposition(|a| a.qids.contains(&qid) && !a.answers.contains_key(&qid))?;
    let a = &mut st.asking[i];
    a.answers.insert(qid, text.trim().to_string());
    if !a.qids.iter().all(|q| a.answers.contains_key(q)) {
        return None;
    }
    let a = st.asking.remove(i);
    let given: std::collections::BTreeMap<String, Vec<String>> =
        a.answers.into_iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k, vec![v])).collect();
    let map: serde_json::Map<String, Value> = given.iter().map(|(k, v)| (k.clone(), json!({"answers": v}))).collect();
    let how = if given.is_empty() { "skipped" } else { "answered" };
    Some((
        jsonrpc::response(&a.id, json!({"answers": map})),
        Ev::QuestionDone { request: a.request, answers: given, how: how.into() },
    ))
}

/// The question requests whose wait is over: each answered with nothing (the agent goes on by itself),
/// as replies to write and events saying so.
fn expire(st: &mut State, now: tokio::time::Instant) -> Vec<(String, Ev)> {
    let (over, keep): (Vec<_>, Vec<_>) =
        std::mem::take(&mut st.asking).into_iter().partition(|a| a.deadline.is_some_and(|d| d <= now));
    st.asking = keep;
    over.into_iter()
        .map(|a| {
            (
                jsonrpc::response(&a.id, json!({"answers": {}})),
                Ev::QuestionDone { request: a.request, answers: Default::default(), how: "expired".into() },
            )
        })
        .collect()
}

/// Waits until the earliest question deadline (forever when there is none).
async fn next_deadline(st: &State) {
    match st.asking.iter().filter_map(|a| a.deadline).min() {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

/// A steer for the running turn: the request to write, nothing yet (waiting for the turn id), or
/// (with no turn running) the refusal to report.
fn steer(st: &mut State, id: String, text: String, images: Vec<Image>) -> Result<Option<String>, Box<Out>> {
    match (st.thread.clone(), st.turn.clone()) {
        (Some(thread), Some(turn)) => Ok(Some(st.request(
            "turn/steer",
            json!({
                "threadId": thread, "expectedTurnId": turn, "clientUserMessageId": id,
                "input": input_items(st, &text, &images),
            }),
            Pending::Steer { id },
        ))),
        _ if st.starting => {
            st.steer_wait.push((id, text, images));
            Ok(None)
        }
        _ => Err(Box::new(Out::SteerRejected { id, reason: "no turn is running".into() })),
    }
}

fn turn_start(st: &mut State, text: &str, images: &[Image]) -> String {
    st.starting = true;
    st.final_text = None;
    st.last_error = None;
    st.streamed.clear();
    st.commands.clear();
    let thread = st.thread.clone().unwrap_or_default();
    st.request("turn/start", json!({"threadId": thread, "input": input_items(st, text, images)}), Pending::TurnStart)
}

fn thread_start(st: &mut State, opts: &Opts) -> String {
    let mut p = json!({"cwd": opts.cwd, "approvalPolicy": "never", "sandbox": "danger-full-access"});
    if let Some(m) = &opts.model {
        p["model"] = json!(m);
    }
    st.request("thread/start", p, Pending::Thread { resumed: false })
}

/// Translates one line; returns lines to write back and messages for the session.
fn handle(st: &mut State, line: &str, opts: &Opts) -> (Vec<String>, Vec<Out>) {
    let (mut writes, mut outs) = (Vec::new(), Vec::new());
    let Some(msg) = jsonrpc::parse(line) else { return (writes, outs) };
    match msg {
        Incoming::Response { id, result } => match (st.pending.remove(&id), result) {
            (Some(Pending::Initialize), Ok(_)) => {
                writes.push(jsonrpc::notification("initialized", None));
                match &opts.resume {
                    Some(thread) => {
                        let mut p = json!({"threadId": thread});
                        if let Some(m) = &opts.model {
                            p["model"] = json!(m);
                        }
                        writes.push(st.request("thread/resume", p, Pending::Thread { resumed: true }));
                    }
                    None => writes.push(thread_start(st, opts)),
                }
            }
            (Some(Pending::Initialize), Err(e)) => outs.push(Out::Exited(format!("Codex refused to start: {e}"))),
            (Some(Pending::Thread { .. }), Ok(r)) => {
                if let Some(id) = r["thread"]["id"].as_str() {
                    st.thread = Some(id.to_string());
                    st.ready = true;
                    outs.push(Out::Session(id.to_string()));
                    while let Some((text, images)) = st.waiting.pop_front() {
                        writes.push(turn_start(st, &text, &images));
                    }
                    writes.push(st.request("skills/list", json!({"cwds": [opts.cwd]}), Pending::Skills));
                } else {
                    outs.push(Out::Exited("Codex did not return a thread".into()));
                }
            }
            (Some(Pending::Thread { resumed: true }), Err(e)) => {
                outs.push(Out::Ev(Ev::Notice {
                    input_id: None,
                    level: "warn",
                    code: "context_lost",
                    text: format!("Codex could not continue the earlier conversation ({e}); a new one starts"),
                }));
                writes.push(thread_start(st, opts));
            }
            (Some(Pending::Thread { resumed: false }), Err(e)) => {
                outs.push(Out::Exited(format!("Codex could not start a thread: {e}")))
            }
            (Some(Pending::TurnStart), Ok(r)) => {
                st.starting = false;
                st.turn = r["turn"]["id"].as_str().map(str::to_string);
                for (id, text, images) in std::mem::take(&mut st.steer_wait) {
                    match steer(st, id, text, images) {
                        Ok(Some(line)) => writes.push(line),
                        Ok(None) => {}
                        Err(o) => outs.push(*o),
                    }
                }
            }
            (Some(Pending::TurnStart), Err(e)) => {
                st.starting = false;
                for (id, _, _) in std::mem::take(&mut st.steer_wait) {
                    outs.push(Out::SteerRejected { id, reason: "the turn did not start".into() });
                }
                outs.push(Out::TurnEnd { status: TurnStatus::Failed, final_text: None, reason: Some(e) })
            }
            (Some(Pending::Skills), Ok(r)) => {
                let skills: Vec<&Value> = r["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|e| e["skills"].as_array().into_iter().flatten())
                    .filter(|k| k["enabled"].as_bool() != Some(false))
                    .collect();
                st.skills = skills
                    .iter()
                    .filter_map(|k| Some((k["name"].as_str()?.to_string(), k["path"].as_str()?.to_string())))
                    .collect();
                let commands = skills
                    .iter()
                    .filter_map(|k| {
                        let name = k["name"].as_str()?;
                        let about = k["interface"]["shortDescription"]
                            .as_str()
                            .or(k["shortDescription"].as_str())
                            .or(k["description"].as_str())
                            .unwrap_or("");
                        Some(SlashCommand::new("skill", name, format!("${name}"), about, None))
                    })
                    .collect();
                outs.push(Out::Ev(Ev::Commands { commands }));
            }
            (Some(Pending::Terminals { items }), Ok(r)) => {
                let thread = st.thread.clone().unwrap_or_default();
                for t in r["data"].as_array().into_iter().flatten() {
                    if let (Some(item), Some(pid)) = (t["itemId"].as_str(), t["processId"].as_str())
                        && items.contains(item)
                    {
                        tracing::info!(
                            command = t["command"].as_str().unwrap_or(""),
                            "stopping a command of the interrupted turn"
                        );
                        writes.push(st.request(
                            "thread/backgroundTerminals/terminate",
                            json!({"threadId": thread, "processId": pid}),
                            Pending::Other,
                        ));
                    }
                }
            }
            // A Codex without the method: its commands keep running, as they did before.
            (Some(Pending::Terminals { .. }), Err(e)) => {
                tracing::debug!(error = %e, "cannot list Codex's background terminals")
            }
            (Some(Pending::Steer { id }), Ok(_)) => outs.push(Out::Steered { id }),
            (Some(Pending::Steer { id }), Err(e)) => outs.push(Out::SteerRejected { id, reason: e }),
            (Some(Pending::Other), Err(e)) => {
                outs.push(Out::Ev(Ev::Notice { input_id: None, level: "warn", code: "rpc_error", text: e }))
            }
            _ => {}
        },
        Incoming::Request { id, method, params } => {
            if params["threadId"].as_str().is_some_and(|t| Some(t) != st.thread.as_deref()) {
                writes.push(jsonrpc::error(&id, -32602, "unknown thread"));
                return (writes, outs);
            }
            match method.as_str() {
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                    writes.push(jsonrpc::response(&id, json!({"decision": "accept"})))
                }
                "execCommandApproval" | "applyPatchApproval" => {
                    writes.push(jsonrpc::response(&id, json!({"decision": "approved"})))
                }
                "item/permissions/requestApproval" => {
                    writes.push(jsonrpc::response(&id, json!({"permissions": params["permissions"], "scope": "turn"})))
                }
                "mcpServer/elicitation/request" => {
                    writes.push(jsonrpc::response(&id, json!({"action": "decline", "content": null})))
                }
                "item/tool/requestUserInput" => {
                    let request = request_name(st, &id);
                    let blocking = params["isBlocking"].as_bool();
                    let waits = blocking != Some(true);
                    let mut qids = Vec::new();
                    for q in params["questions"].as_array().into_iter().flatten() {
                        let qid = q["id"].as_str().unwrap_or("").to_string();
                        let header = q["header"].as_str().map(str::trim).filter(|h| !h.is_empty()).map(str::to_string);
                        let opts = q["options"].as_array().map(Vec::as_slice).unwrap_or_default();
                        let options = opts.iter().filter_map(|o| o["label"].as_str().map(str::to_string)).collect();
                        let mut descriptions: Vec<String> = opts
                            .iter()
                            .filter(|o| o["label"].is_string())
                            .map(|o| o["description"].as_str().unwrap_or("").trim().to_string())
                            .collect();
                        if descriptions.iter().all(String::is_empty) {
                            descriptions.clear();
                        }
                        qids.push(qid.clone());
                        outs.push(Out::Ev(Ev::Question {
                            qid,
                            prompt: q["question"].as_str().unwrap_or("").to_string(),
                            options,
                            request: Some(request.clone()),
                            item_id: params["itemId"].as_str().map(str::to_string),
                            header,
                            descriptions,
                            other: q["isOther"] == true,
                            secret: q["isSecret"] == true,
                            blocking,
                            expires_in_ms: waits.then_some(QUESTION_WAIT.as_millis() as u64),
                        }));
                    }
                    if qids.is_empty() {
                        writes.push(jsonrpc::response(&id, json!({"answers": {}})));
                    } else {
                        let deadline = waits.then(|| tokio::time::Instant::now() + QUESTION_WAIT);
                        st.asking.push(Asking { id, request, qids, answers: HashMap::new(), deadline });
                    }
                }
                other => writes.push(jsonrpc::unsupported(&id, other)),
            }
        }
        Incoming::Notification { method, params } => {
            if params["threadId"].as_str().is_some_and(|t| Some(t) != st.thread.as_deref()) {
                return (writes, outs);
            }
            notification(st, &method, &params, &mut writes, &mut outs);
        }
    }
    (writes, outs)
}

fn notification(st: &mut State, method: &str, p: &Value, writes: &mut Vec<String>, outs: &mut Vec<Out>) {
    match method {
        "item/agentMessage/delta" => {
            let id = p["itemId"].as_str().unwrap_or("").to_string();
            st.streamed.insert(id.clone());
            let phase = st.phases.get(&id).cloned();
            outs.push(Out::Ev(Ev::Message { id, text: p["delta"].as_str().unwrap_or("").into(), delta: true, phase }));
        }
        "item/reasoning/summaryTextDelta" => {
            let id = p["itemId"].as_str().unwrap_or("").to_string();
            st.streamed.insert(id.clone());
            outs.push(Out::Ev(Ev::Thought { id, text: p["delta"].as_str().unwrap_or("").into(), delta: true }));
        }
        // A new paragraph of the summary.
        "item/reasoning/summaryPartAdded" => {
            let id = p["itemId"].as_str().unwrap_or("").to_string();
            if p["summaryIndex"].as_u64().unwrap_or(0) > 0 && st.streamed.contains(&id) {
                outs.push(Out::Ev(Ev::Thought { id, text: "\n\n".into(), delta: true }));
            }
        }
        "serverRequest/resolved" => {
            if let Some(i) = st.asking.iter().position(|a| a.id == p["requestId"]) {
                let a = st.asking.remove(i);
                outs.push(Out::Ev(Ev::QuestionDone {
                    request: a.request,
                    answers: Default::default(),
                    how: "resolved".into(),
                }));
            }
        }
        "item/started" => {
            if p["item"]["type"] == "commandExecution"
                && let Some(id) = p["item"]["id"].as_str()
            {
                st.commands.insert(id.to_string());
            }
            if p["item"]["type"] == "agentMessage"
                && let (Some(id), Some(phase)) = (p["item"]["id"].as_str(), p["item"]["phase"].as_str())
            {
                st.phases.insert(id.to_string(), phase.to_string());
            }
            if let Some(ev) = item_started(&p["item"]) {
                if matches!(&ev, Ev::ToolCall { content, .. } if !content.is_empty()) {
                    st.diffed.insert(p["item"]["id"].as_str().unwrap_or("").to_string());
                }
                outs.push(Out::Ev(ev));
            }
        }
        "item/commandExecution/outputDelta" => {
            let id = p["itemId"].as_str().unwrap_or("").to_string();
            let delta = p["delta"].as_str().unwrap_or("");
            let sent = st.output.entry(id.clone()).or_default();
            if *sent >= PREVIEW || delta.is_empty() {
                return;
            }
            let (text, truncated) = cut(delta, PREVIEW - *sent);
            *sent = if truncated { PREVIEW } else { *sent + text.len() };
            outs.push(Out::Ev(Ev::ToolUpdate {
                call_id: id,
                status: None,
                content: vec![ToolContent::Text { text }],
                truncated,
                exit_code: None,
                duration_ms: None,
            }));
        }
        "item/fileChange/patchUpdated" => {
            let id = p["itemId"].as_str().unwrap_or("").to_string();
            let content = diffs(&p["changes"]);
            if !content.is_empty() && st.diffed.insert(id.clone()) {
                outs.push(Out::Ev(Ev::ToolUpdate {
                    call_id: id,
                    status: None,
                    content,
                    truncated: false,
                    exit_code: None,
                    duration_ms: None,
                }));
            }
        }
        "item/mcpToolCall/progress" => {
            if let Some(m) = p["message"].as_str().filter(|m| !m.is_empty()) {
                outs.push(Out::Ev(Ev::ToolUpdate {
                    call_id: p["itemId"].as_str().unwrap_or("").to_string(),
                    status: None,
                    content: vec![ToolContent::Text { text: format!("{}\n", cut(m, 400).0) }],
                    truncated: false,
                    exit_code: None,
                    duration_ms: None,
                }));
            }
        }
        "turn/diff/updated" => st.turn_diff = p["diff"].as_str().map(str::to_string),
        "thread/compacted" => outs.push(Out::Ev(Ev::Notice {
            input_id: None,
            level: "info",
            code: "compacted",
            text: "Codex summarized the conversation so far to make room in its context".into(),
        })),
        "item/completed" => item_completed(st, &p["item"], outs),
        "turn/plan/updated" => {
            let entries = p["plan"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| PlanEntry {
                    content: e["step"].as_str().unwrap_or("").to_string(),
                    status: match e["status"].as_str().unwrap_or("") {
                        "inProgress" => "in_progress".into(),
                        "completed" => "completed".into(),
                        _ => "pending".into(),
                    },
                })
                .collect();
            outs.push(Out::Ev(Ev::Plan { entries }));
        }
        "thread/tokenUsage/updated" => {
            let last = &p["tokenUsage"]["last"];
            outs.push(Out::Ev(Ev::Usage {
                input_tokens: last["inputTokens"].as_u64(),
                output_tokens: last["outputTokens"].as_u64(),
                cost_usd: None,
                context_window: p["tokenUsage"]["modelContextWindow"].as_u64(),
                context_used: last["totalTokens"].as_u64(),
                cache_read_tokens: last["cachedInputTokens"].as_u64(),
                cache_write_tokens: last["cacheWriteInputTokens"].as_u64().filter(|n| *n > 0),
                reasoning_tokens: last["reasoningOutputTokens"].as_u64(),
            }));
        }
        "error" => st.last_error = Some(provider_message(p["error"]["message"].as_str().unwrap_or("error"))),
        "turn/completed" => {
            let turn = &p["turn"];
            st.turn = None;
            st.starting = false;
            st.output.clear();
            st.diffed.clear();
            st.phases.clear();
            if let Some(diff) = st.turn_diff.take().filter(|d| !d.trim().is_empty()) {
                let (diff, truncated) = cut(&diff, TURN_DIFF);
                outs.push(Out::Ev(Ev::TurnDiff { diff, truncated }));
            }
            let status = match turn["status"].as_str().unwrap_or("") {
                "completed" => TurnStatus::Completed,
                "interrupted" => TurnStatus::Interrupted,
                _ => TurnStatus::Failed,
            };
            // Questions still waiting end with their turn.
            for a in std::mem::take(&mut st.asking) {
                let how = if status == TurnStatus::Interrupted { "dismissed" } else { "resolved" };
                outs.push(Out::Ev(Ev::QuestionDone {
                    request: a.request,
                    answers: Default::default(),
                    how: how.into(),
                }));
            }
            let commands = std::mem::take(&mut st.commands);
            if status == TurnStatus::Interrupted
                && !commands.is_empty()
                && let Some(thread) = st.thread.clone()
            {
                writes.push(st.request(
                    "thread/backgroundTerminals/list",
                    json!({"threadId": thread, "limit": 100}),
                    Pending::Terminals { items: commands },
                ));
            }
            let reason = turn["error"]["message"].as_str().map(provider_message).or_else(|| st.last_error.take());
            let status = if status == TurnStatus::Completed && reason.is_some() && st.final_text.is_none() {
                TurnStatus::Failed
            } else {
                status
            };
            outs.push(Out::TurnEnd {
                status,
                final_text: st.final_text.take(),
                reason: if status == TurnStatus::Completed { None } else { reason },
            });
        }
        _ => {}
    }
}

fn item_started(item: &Value) -> Option<Ev> {
    let call_id = item["id"].as_str()?.to_string();
    let (kind, title, content, input) = match item["type"].as_str()? {
        "commandExecution" => {
            let raw = item["command"].as_str().unwrap_or("command");
            let command = display_command(raw);
            // Codex's own reading of the command (what it reads, searches or lists), which its
            // clients show instead of the command line.
            let actions: Vec<Value> = item["commandActions"]
                .as_array()
                .into_iter()
                .flatten()
                .take(16)
                .map(|a| {
                    let mut a = preview_value(a);
                    if let Some(c) = a.get_mut("command").and_then(|c| c.as_str().map(|c| cut(c, 400).0)) {
                        a["command"] = Value::String(c);
                    }
                    a
                })
                .collect();
            let mut input = json!({"command": cut(&command, PREVIEW).0, "actions": actions});
            if let Some(cwd) = item["cwd"].as_str() {
                input["cwd"] = Value::String(cwd.to_string());
            }
            (ToolKind::Execute, format!("Shell {command}"), vec![], Some(input))
        }
        "fileChange" => {
            let paths: Vec<&str> =
                item["changes"].as_array().into_iter().flatten().filter_map(|c| c["path"].as_str()).collect();
            let input = json!({"files": files(&item["changes"])});
            (ToolKind::Edit, format!("edit {}", paths.join(", ")), diffs(&item["changes"]), Some(input))
        }
        "mcpToolCall" => (
            ToolKind::Other,
            format!("{}.{}", item["server"].as_str().unwrap_or(""), item["tool"].as_str().unwrap_or("")),
            vec![],
            Some(preview_value(&item["arguments"])),
        ),
        "dynamicToolCall" => (
            ToolKind::Other,
            item["tool"].as_str().unwrap_or("tool").to_string(),
            vec![],
            Some(preview_value(&item["arguments"])),
        ),
        "webSearch" => (ToolKind::Fetch, format!("search {}", item["query"].as_str().unwrap_or("")), vec![], None),
        "imageView" => (ToolKind::Read, format!("view {}", item["path"].as_str().unwrap_or("image")), vec![], None),
        "imageGeneration" => (ToolKind::Other, "generate an image".to_string(), vec![], None),
        "collabAgentToolCall" => (
            ToolKind::Think,
            format!("agent {}", item["tool"].as_str().unwrap_or("")),
            vec![],
            item["prompt"].as_str().map(|p| json!({"prompt": cut(p, PREVIEW).0})),
        ),
        _ => return None,
    };
    let locations = item["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["path"].as_str().map(str::to_string))
        .collect();
    Some(Ev::ToolCall {
        call_id,
        parent: None,
        title: cut(&title, 400).0,
        kind,
        status: ToolStatus::InProgress,
        input,
        locations,
        content,
    })
}

fn item_completed(st: &mut State, item: &Value, outs: &mut Vec<Out>) {
    let id = item["id"].as_str().unwrap_or("").to_string();
    match item["type"].as_str().unwrap_or("") {
        "agentMessage" => {
            let text = item["text"].as_str().unwrap_or("").to_string();
            let phase = item["phase"].as_str().map(str::to_string);
            if !st.streamed.contains(&id) && !text.is_empty() {
                outs.push(Out::Ev(Ev::Message {
                    id: id.clone(),
                    text: text.clone(),
                    delta: false,
                    phase: phase.clone(),
                }));
            } else if st.streamed.contains(&id) && phase.is_some() && !st.phases.contains_key(&id) {
                // It streamed without saying where it belongs: an empty piece carries the phase it ended
                // with (a viewer that does not know phases appends nothing, and nothing is shown twice).
                outs.push(Out::Ev(Ev::Message {
                    id: id.clone(),
                    text: String::new(),
                    delta: true,
                    phase: phase.clone(),
                }));
            }
            if let Some(phase) = phase.clone() {
                st.phases.insert(id, phase);
            }
            if phase.as_deref() != Some("commentary") {
                st.final_text = Some(text);
            }
        }
        "reasoning" => {
            let text: Vec<&str> = item["summary"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            if !st.streamed.contains(&id) && !text.is_empty() {
                outs.push(Out::Ev(Ev::Thought { id, text: text.join("\n\n"), delta: false }));
            }
        }
        "plan" => {
            // The plan Codex proposes in plan mode (its deltas may not add up to it, so only the
            // complete text is shown).
            let text = item["text"].as_str().unwrap_or("");
            if !text.is_empty() {
                outs.push(Out::Ev(Ev::Message { id, text: text.to_string(), delta: false, phase: None }));
            }
        }
        "contextCompaction" => {}
        "commandExecution"
        | "fileChange"
        | "mcpToolCall"
        | "dynamicToolCall"
        | "webSearch"
        | "imageView"
        | "imageGeneration"
        | "collabAgentToolCall" => {
            let status = match item["status"].as_str().unwrap_or("completed") {
                "failed" | "declined" | "errored" => ToolStatus::Failed,
                "inProgress" => ToolStatus::InProgress,
                _ => ToolStatus::Completed,
            };
            let status = if item["success"].as_bool() == Some(false) { ToolStatus::Failed } else { status };
            let mut content = Vec::new();
            if item["type"] == "fileChange" && !st.diffed.contains(&id) {
                content = diffs(&item["changes"]);
            }
            // Output that streamed is not sent again.
            let streamed = st.output.get(&id).copied();
            let mut text = if streamed.is_some() {
                String::new()
            } else {
                item["aggregatedOutput"].as_str().unwrap_or("").to_string()
            };
            if let Some(code) = item["exitCode"].as_i64()
                && code != 0
            {
                text.push_str(&format!("\n(exit code {code})"));
            }
            if text.is_empty()
                && let Some(r) = item.get("result").filter(|r| r.is_object())
            {
                text = super::content_text(&r["content"]);
            }
            if text.is_empty()
                && let Some(items) = item.get("contentItems").filter(|c| c.is_array())
            {
                text = super::content_text(items);
            }
            if text.is_empty() && item["type"] == "webSearch" {
                let a = &item["action"];
                text = match a["type"].as_str().unwrap_or("") {
                    "openPage" => format!("opened {}", a["url"].as_str().unwrap_or("")),
                    "findInPage" => format!(
                        "found '{}' in {}",
                        a["pattern"].as_str().unwrap_or(""),
                        a["url"].as_str().unwrap_or("")
                    ),
                    _ => a["query"]
                        .as_str()
                        .or(item["query"].as_str())
                        .map(|q| format!("searched {q}"))
                        .unwrap_or_default(),
                };
            }
            if text.is_empty()
                && let Some(p) = item["savedPath"].as_str()
            {
                text = format!("saved {p}");
            }
            if text.is_empty()
                && let Some(e) = item["error"]["message"].as_str()
            {
                text = e.to_string();
            }
            let (text, truncated) = if streamed.is_some() {
                // Only what was not streamed: the exit code line.
                (text.trim_start().to_string(), streamed.is_some_and(|n| n >= PREVIEW))
            } else {
                cut(text.trim(), PREVIEW)
            };
            if !text.is_empty() {
                content.push(ToolContent::Text { text });
            }
            outs.push(Out::Ev(Ev::ToolUpdate {
                call_id: id,
                status: Some(status),
                content,
                truncated,
                exit_code: item["exitCode"].as_i64(),
                duration_ms: item["durationMs"].as_u64(),
            }));
        }
        _ => {}
    }
}

/// The diffs of a file change, one per hunk (old and new text of the hunk), so they show as
/// diffs with the right counts of added and removed lines.
fn diffs(changes: &Value) -> Vec<ToolContent> {
    let mut out = Vec::new();
    let mut total = 0;
    for c in changes.as_array().into_iter().flatten() {
        if total >= DIFF_TOTAL {
            break;
        }
        let path = c["path"].as_str().unwrap_or("").to_string();
        let kind = c["kind"]["type"].as_str().unwrap_or("update");
        let diff = c["diff"].as_str().unwrap_or("");
        let hunks = hunks(diff);
        if hunks.is_empty() {
            // An added file comes as its content, a deleted one may come empty.
            let (old, new) = match kind {
                "add" => (None, diff.to_string()),
                "delete" => (Some(diff.to_string()), String::new()),
                _ => continue,
            };
            let (old, new) = (old.map(|o| cut(&o, DIFF_SIDE).0), cut(&new, DIFF_SIDE).0);
            total += old.as_ref().map_or(0, String::len) + new.len();
            out.push(ToolContent::Diff { path, old, new });
        } else {
            for (old, new) in hunks {
                let (old, new) = ((kind != "add").then(|| cut(&old, DIFF_SIDE).0), cut(&new, DIFF_SIDE).0);
                total += old.as_ref().map_or(0, String::len) + new.len();
                out.push(ToolContent::Diff { path: path.clone(), old, new });
                if total >= DIFF_TOTAL {
                    break;
                }
            }
        }
        if out.len() >= 40 {
            break;
        }
    }
    out
}

/// An error's message: often the provider's JSON, whose inner message reads better.
fn provider_message(m: &str) -> String {
    serde_json::from_str::<Value>(m)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| m.to_string())
}

/// The files of a file change, for clients that show each file once: its path, what happened to it
/// (`add`, `delete`, `update`), where it moved, and its unified diff (an added file's diff is its
/// content).
fn files(changes: &Value) -> Vec<Value> {
    // Every path is listed; the diffs share one budget (they also come, per hunk, as the call's content).
    let mut budget = FILES_DIFF;
    changes
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_FILES)
        .map(|c| {
            let raw = c["diff"].as_str().unwrap_or("");
            // Past the budget a file keeps its path only (the controller falls back to the content).
            let (diff, cut_short) =
                if budget == 0 { (String::new(), !raw.is_empty()) } else { cut(raw, DIFF_SIDE.min(budget)) };
            budget = budget.saturating_sub(diff.len());
            let mut f = json!({
                "path": c["path"].as_str().unwrap_or(""),
                "kind": c["kind"]["type"].as_str().unwrap_or("update"),
                "diff": diff,
            });
            if cut_short {
                f["truncated"] = Value::Bool(true);
            }
            if let Some(to) = c["kind"]["move_path"].as_str() {
                f["move_path"] = Value::String(to.to_string());
            }
            f
        })
        .collect()
}

/// Most files of one change listed in its input, and how much of their diffs it carries in all.
const MAX_FILES: usize = 1000;
const FILES_DIFF: usize = 512 * 1024;

/// A command as the user would type it: Codex runs each one through a shell (`/bin/zsh -lc 'ls -la'`,
/// `powershell.exe -Command ...`); the wrapper is left out.
pub(crate) fn display_command(raw: &str) -> String {
    let words = split_words(raw);
    let program = |w: &str| {
        let base = w.rsplit(['/', '\\']).next().unwrap_or(w).to_ascii_lowercase();
        base.strip_suffix(".exe").map(str::to_string).unwrap_or(base)
    };
    let Some(first) = words.first() else { return raw.to_string() };
    let rest = |from: usize| words.get(from..).map(|w| w.join(" ")).unwrap_or_default();
    match program(first).as_str() {
        "bash" | "zsh" | "sh" | "dash" | "fish" | "ksh" => {
            // `-lc`, `-c`, or `-l -c`, then the script as one word.
            let flags: Vec<&String> = words[1..].iter().take_while(|w| w.starts_with('-')).collect();
            let script_at = 1 + flags.len();
            if flags.iter().any(|f| f.contains('c')) && words.len() == script_at + 1 {
                return words[script_at].trim().to_string();
            }
        }
        "powershell" | "pwsh" => {
            let at = words.iter().position(|w| {
                let w = w.to_ascii_lowercase();
                w == "-command" || w == "-c"
            });
            if let Some(at) = at
                && at + 1 < words.len()
            {
                return rest(at + 1).trim().to_string();
            }
        }
        "cmd" => {
            if let Some(at) = words.iter().position(|w| w.eq_ignore_ascii_case("/c"))
                && at + 1 < words.len()
            {
                return rest(at + 1).trim().to_string();
            }
        }
        _ => {}
    }
    raw.to_string()
}

/// A command line split into words the way a POSIX shell quotes them (single quotes, double quotes
/// with backslash escapes, backslashes outside quotes).
fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                started = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    cur.push(c);
                }
            }
            '"' => {
                started = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) => {
                            cur.push(chars.next().expect("peeked"))
                        }
                        c => cur.push(c),
                    }
                }
            }
            '\\' if !cfg!(windows) || chars.peek().is_some_and(|n| *n == ' ' || *n == '\'' || *n == '"') => {
                started = true;
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                started = true;
                cur.push(c);
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// The hunks of a unified diff as (old text, new text).
fn hunks(diff: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut cur: Option<(String, String)> = None;
    for line in diff.lines() {
        if line.starts_with("@@") {
            out.extend(cur.take());
            cur = Some((String::new(), String::new()));
            continue;
        }
        let Some((old, new)) = cur.as_mut() else { continue };
        let (sign, rest) = line.split_at(line.len().min(1));
        match sign {
            " " => {
                old.push_str(rest);
                old.push('\n');
                new.push_str(rest);
                new.push('\n');
            }
            "-" => {
                old.push_str(rest);
                old.push('\n');
            }
            "+" => {
                new.push_str(rest);
                new.push('\n');
            }
            "" => {
                old.push('\n');
                new.push('\n');
            }
            _ => {}
        }
    }
    out.extend(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(resume: Option<&str>) -> Opts {
        Opts { cwd: "/w".into(), model: Some("m1".into()), resume: resume.map(str::to_string) }
    }

    #[test]
    fn handshake_starts_a_bypassed_thread_and_flushes_waiting_messages() {
        let mut st = State::default();
        let _ = st.request("initialize", json!({}), Pending::Initialize);
        st.waiting.push_back(("hi".into(), vec![]));
        let (w, _) = handle(&mut st, r#"{"id":1,"result":{}}"#, &opts(None));
        assert!(w[0].contains("\"initialized\""));
        let start: Value = serde_json::from_str(&w[1]).unwrap();
        assert_eq!(start["method"], "thread/start");
        assert_eq!(start["params"]["approvalPolicy"], "never");
        assert_eq!(start["params"]["sandbox"], "danger-full-access");
        assert_eq!(start["params"]["model"], "m1");
        let (w, o) = handle(&mut st, r#"{"id":2,"result":{"thread":{"id":"th"}}}"#, &opts(None));
        assert!(matches!(&o[0], Out::Session(s) if s == "th"));
        let turn: Value = serde_json::from_str(&w[0]).unwrap();
        assert_eq!(
            (turn["method"].as_str(), turn["params"]["input"][0]["text"].as_str()),
            (Some("turn/start"), Some("hi"))
        );
        handle(&mut st, r#"{"id":3,"result":{"turn":{"id":"tu"}}}"#, &opts(None));
        assert_eq!(st.turn.as_deref(), Some("tu"));
    }

    #[test]
    fn an_interrupted_turn_stops_only_its_own_background_commands() {
        let mut st = State { thread: Some("th".into()), ready: true, ..Default::default() };
        let _ = st.request("initialize", json!({}), Pending::Initialize);
        // A command of an earlier, completed turn keeps running (a server the user asked for).
        for l in [
            r#"{"method":"item/started","params":{"threadId":"th","item":{"type":"commandExecution","id":"old","command":"serve"}}}"#,
            r#"{"method":"turn/completed","params":{"threadId":"th","turn":{"id":"t1","status":"completed"}}}"#,
        ] {
            let (w, _) = handle(&mut st, l, &opts(None));
            assert!(w.is_empty(), "a completed turn stops nothing: {w:?}");
        }
        let _ = turn_start(&mut st, "go", &[]);
        handle(
            &mut st,
            r#"{"method":"item/started","params":{"threadId":"th","item":{"type":"commandExecution","id":"c2","command":"sleep 45"}}}"#,
            &opts(None),
        );
        let (w, o) = handle(
            &mut st,
            r#"{"method":"turn/completed","params":{"threadId":"th","turn":{"id":"t2","status":"interrupted"}}}"#,
            &opts(None),
        );
        assert!(matches!(&o[0], Out::TurnEnd { status: TurnStatus::Interrupted, .. }));
        let list: Value = serde_json::from_str(&w[0]).unwrap();
        assert_eq!(list["method"], "thread/backgroundTerminals/list");
        let id = list["id"].as_u64().unwrap();
        let answer = json!({"id": id, "result": {"data": [
            {"itemId": "old", "processId": "11", "command": "serve", "cwd": "/w"},
            {"itemId": "c2", "processId": "12", "command": "sleep 45", "cwd": "/w"},
        ], "nextCursor": null}});
        let (w, _) = handle(&mut st, &answer.to_string(), &opts(None));
        assert_eq!(w.len(), 1, "{w:?}");
        let stop: Value = serde_json::from_str(&w[0]).unwrap();
        assert_eq!(stop["method"], "thread/backgroundTerminals/terminate");
        assert_eq!(stop["params"], json!({"threadId": "th", "processId": "12"}));
        // A Codex without the method answers with an error: nothing else happens.
        let _ = turn_start(&mut st, "again", &[]);
        handle(
            &mut st,
            r#"{"method":"item/started","params":{"threadId":"th","item":{"type":"commandExecution","id":"c3","command":"x"}}}"#,
            &opts(None),
        );
        let (w, _) = handle(
            &mut st,
            r#"{"method":"turn/completed","params":{"threadId":"th","turn":{"id":"t3","status":"interrupted"}}}"#,
            &opts(None),
        );
        let id = serde_json::from_str::<Value>(&w[0]).unwrap()["id"].as_u64().unwrap();
        let (w, o) = handle(
            &mut st,
            &json!({"id": id, "error": {"code": -32601, "message": "unknown method"}}).to_string(),
            &opts(None),
        );
        assert!(w.is_empty() && o.is_empty(), "{w:?} {o:?}");
    }

    #[test]
    fn a_resume_that_fails_starts_a_new_thread() {
        let mut st = State::default();
        let _ = st.request("initialize", json!({}), Pending::Initialize);
        let (w, _) = handle(&mut st, r#"{"id":1,"result":{}}"#, &opts(Some("old")));
        assert!(w[1].contains("thread/resume") && w[1].contains("\"old\""));
        let (w, o) =
            handle(&mut st, r#"{"id":2,"error":{"code":-1,"message":"no rollout found"}}"#, &opts(Some("old")));
        assert!(matches!(&o[0], Out::Ev(Ev::Notice { code: "context_lost", .. })));
        assert!(w[0].contains("thread/start"));
    }

    #[test]
    fn notifications_become_events_and_the_turn_ends() {
        let mut st = State { thread: Some("th".into()), ready: true, ..Default::default() };
        let lines = [
            r#"{"method":"item/agentMessage/delta","params":{"threadId":"th","itemId":"a1","delta":"ON"}}"#,
            r#"{"method":"item/agentMessage/delta","params":{"threadId":"other","itemId":"zz","delta":"no"}}"#,
            r#"{"method":"item/started","params":{"threadId":"th","item":{"type":"commandExecution","id":"c1","command":"/bin/zsh -lc ls","status":"inProgress"}}}"#,
            r#"{"method":"item/completed","params":{"threadId":"th","item":{"type":"commandExecution","id":"c1","status":"failed","aggregatedOutput":"boom","exitCode":2}}}"#,
            r#"{"method":"item/completed","params":{"threadId":"th","item":{"type":"agentMessage","id":"a1","text":"ONE","phase":"final_answer"}}}"#,
            r#"{"method":"turn/completed","params":{"threadId":"th","turn":{"id":"tu","status":"completed","error":null}}}"#,
        ];
        let mut outs = Vec::new();
        for l in lines {
            outs.extend(handle(&mut st, l, &opts(None)).1);
        }
        let shown: Vec<String> = outs.iter().map(|o| format!("{o:?}")).collect();
        assert_eq!(outs.len(), 5, "{shown:?}");
        assert!(
            matches!(&outs[1], Out::Ev(Ev::ToolCall { kind: ToolKind::Execute, title, input: Some(i), .. }) if title == "Shell ls" && i["command"] == "ls")
        );
        assert!(
            matches!(&outs[2], Out::Ev(Ev::ToolUpdate { status: Some(ToolStatus::Failed), content, exit_code: Some(2), .. }) if matches!(&content[0], ToolContent::Text { text } if text.contains("exit code 2")))
        );
        // The message streamed before it said where it belongs: an empty piece carries its phase.
        assert!(
            matches!(&outs[3], Out::Ev(Ev::Message { text, delta: true, phase: Some(p), .. }) if text.is_empty() && p == "final_answer")
        );
        assert!(
            matches!(&outs[4], Out::TurnEnd { status: TurnStatus::Completed, final_text: Some(t), .. } if t == "ONE")
        );
    }

    #[test]
    fn commands_files_and_messages_carry_what_codex_says_about_them() {
        let mut st = State { thread: Some("th".into()), ready: true, ..Default::default() };
        let item = |v: Value| json!({"method": "item/started", "params": {"threadId": "th", "item": v}}).to_string();
        let done = |v: Value| json!({"method": "item/completed", "params": {"threadId": "th", "item": v}}).to_string();
        let lines = [
            item(json!({"type": "agentMessage", "id": "m1", "text": "", "phase": "commentary"})),
            r#"{"method":"item/agentMessage/delta","params":{"threadId":"th","itemId":"m1","delta":"Looking"}}"#
                .to_string(),
            done(json!({"type": "agentMessage", "id": "m1", "text": "Looking", "phase": "commentary"})),
            item(json!({"type": "commandExecution", "id": "c1", "command": "/bin/zsh -lc 'sed -n 1,80p src/app.ts'",
                "cwd": "/w", "status": "inProgress",
                "commandActions": [{"type": "read", "command": "sed -n 1,80p src/app.ts", "name": "app.ts", "path": "/w/src/app.ts"}]})),
            done(
                json!({"type": "commandExecution", "id": "c1", "status": "completed", "aggregatedOutput": "x", "exitCode": 0, "durationMs": 1234}),
            ),
            item(json!({"type": "fileChange", "id": "f1", "status": "inProgress", "changes": [
                {"path": "/w/a.txt", "kind": {"type": "update", "move_path": null}, "diff": "@@ -1,2 +1,2 @@\n keep\n-old\n+new\n"},
                {"path": "/w/b.txt", "kind": {"type": "add"}, "diff": "hello\n"},
                {"path": "/w/c.txt", "kind": {"type": "update", "move_path": "/w/d.txt"}, "diff": ""}]})),
            item(json!({"type": "webSearch", "id": "w1", "query": "rust 1.95"})),
        ];
        let mut outs = Vec::new();
        for l in &lines {
            outs.extend(handle(&mut st, l, &opts(None)).1);
        }
        let shown: Vec<String> = outs.iter().map(|o| format!("{o:?}")).collect();
        // The commentary streamed with its phase, so it is not sent again; it is not the turn's answer.
        assert!(
            matches!(&outs[0], Out::Ev(Ev::Message { delta: true, phase: Some(p), .. }) if p == "commentary"),
            "{shown:?}"
        );
        assert!(st.final_text.is_none());
        let Out::Ev(Ev::ToolCall { title, input: Some(input), .. }) = &outs[1] else { panic!("{shown:?}") };
        assert_eq!(title, "Shell sed -n 1,80p src/app.ts");
        assert_eq!(input["command"], "sed -n 1,80p src/app.ts");
        assert_eq!(input["cwd"], "/w");
        assert_eq!(input["actions"][0]["type"], "read");
        assert_eq!(input["actions"][0]["path"], "/w/src/app.ts");
        assert!(
            matches!(&outs[2], Out::Ev(Ev::ToolUpdate { exit_code: Some(0), duration_ms: Some(1234), .. })),
            "{shown:?}"
        );
        let Out::Ev(Ev::ToolCall { input: Some(input), content, .. }) = &outs[3] else { panic!("{shown:?}") };
        let files = input["files"].as_array().unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!((files[0]["path"].as_str(), files[0]["kind"].as_str()), (Some("/w/a.txt"), Some("update")));
        assert!(files[0]["diff"].as_str().unwrap().contains("+new"));
        assert_eq!(files[1]["kind"], "add");
        assert_eq!(files[2]["move_path"], "/w/d.txt");
        assert_eq!(content.len(), 2, "the moved file has no diff to show");
        // A web search's query is its title (an input would show as JSON in older controllers).
        assert!(matches!(&outs[4], Out::Ev(Ev::ToolCall { title, input: None, .. }) if title == "search rust 1.95"));
    }

    #[test]
    fn a_turn_that_fails_with_the_providers_json_says_its_message() {
        let mut st = State { thread: Some("th".into()), ready: true, ..Default::default() };
        let done = json!({"method": "turn/completed", "params": {"threadId": "th", "turn": {"id": "t", "status": "failed",
            "error": {"message": "{\"type\":\"error\",\"status\":400,\"error\":{\"message\":\"The 'x' model is not supported\"}}"}}}});
        let (_, o) = handle(&mut st, &done.to_string(), &opts(None));
        assert!(
            matches!(&o[0], Out::TurnEnd { status: TurnStatus::Failed, reason: Some(r), .. } if r == "The 'x' model is not supported"),
            "{o:?}"
        );
    }

    #[test]
    fn the_turns_whole_diff_comes_before_its_end_and_big_changes_stay_within_budget() {
        let mut st = State { thread: Some("th".into()), ready: true, ..Default::default() };
        let turn_diff = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-x\n+y\n";
        for l in [
            json!({"method": "turn/diff/updated", "params": {"threadId": "th", "turnId": "t", "diff": "stale"}}),
            json!({"method": "turn/diff/updated", "params": {"threadId": "th", "turnId": "t", "diff": turn_diff}}),
        ] {
            assert!(handle(&mut st, &l.to_string(), &opts(None)).1.is_empty(), "nothing is sent until the turn ends");
        }
        let (_, o) = handle(
            &mut st,
            r#"{"method":"turn/completed","params":{"threadId":"th","turn":{"id":"t","status":"completed"}}}"#,
            &opts(None),
        );
        assert!(matches!(&o[0], Out::Ev(Ev::TurnDiff { diff, truncated: false }) if diff == turn_diff), "{o:?}");
        assert!(matches!(&o[1], Out::TurnEnd { .. }));
        // The next turn starts without it.
        let (_, o) = handle(
            &mut st,
            r#"{"method":"turn/completed","params":{"threadId":"th","turn":{"id":"u","status":"completed"}}}"#,
            &opts(None),
        );
        assert!(matches!(&o[0], Out::TurnEnd { .. }));

        // 60 files of 64 KiB: every path is listed, the diffs keep to their budgets.
        let big = "x\n".repeat(32 * 1024);
        let changes: Vec<Value> =
            (0..60).map(|i| json!({"path": format!("/w/f{i}"), "kind": {"type": "add"}, "diff": big})).collect();
        let item = json!({"method": "item/started", "params": {"threadId": "th", "item": {"type": "fileChange", "id": "f", "status": "inProgress", "changes": changes}}});
        let (_, o) = handle(&mut st, &item.to_string(), &opts(None));
        let Out::Ev(ev @ Ev::ToolCall { input: Some(input), content, .. }) = &o[0] else { panic!("{o:?}") };
        let files = input["files"].as_array().unwrap();
        assert_eq!(files.len(), 60);
        let listed: usize = files.iter().map(|f| f["diff"].as_str().unwrap().len()).sum();
        assert!(listed <= FILES_DIFF + 3, "{listed}");
        assert_eq!(files[59]["truncated"], true);
        let carried: usize = content
            .iter()
            .map(|c| match c {
                ToolContent::Diff { new, .. } => new.len(),
                _ => 0,
            })
            .sum();
        assert!(carried <= DIFF_TOTAL + DIFF_SIDE, "{carried}");
        assert!(serde_json::to_string(ev).unwrap().len() < 4 * 1024 * 1024 - 64 * 1024);
    }

    #[test]
    fn the_shell_around_a_command_is_left_out() {
        assert_eq!(display_command("/bin/zsh -lc 'ls -la'"), "ls -la");
        assert_eq!(display_command("bash -lc \"rg -n \\\"x y\\\" src\""), "rg -n \"x y\" src");
        assert_eq!(display_command("/bin/bash -l -c 'echo it'\\''s'"), "echo it's");
        assert_eq!(display_command("sh -c ls"), "ls");
        assert_eq!(
            display_command(
                "\"C:\\WINDOWS\\System32\\WindowsPowerShell\\v1.0\\powershell.exe\" -NoProfile -Command 'Get-ChildItem src'"
            ),
            "Get-ChildItem src"
        );
        assert_eq!(display_command("cmd.exe /c dir /b"), "dir /b");
        // Not a shell wrapper, or more than a script: shown as it is.
        assert_eq!(display_command("git status"), "git status");
        assert_eq!(display_command("bash script.sh arg"), "bash script.sh arg");
    }

    #[test]
    fn an_unusable_model_fails_the_turn_with_the_providers_message() {
        let mut st = State { thread: Some("th".into()), ready: true, ..Default::default() };
        let err = json!({"method": "error", "params": {"threadId": "th", "error": {"message": "{\"type\":\"error\",\"status\":400,\"error\":{\"message\":\"The 'x' model is not supported\"}}"}}});
        handle(&mut st, &err.to_string(), &opts(None));
        let (_, o) = handle(
            &mut st,
            r#"{"method":"turn/completed","params":{"threadId":"th","turn":{"id":"t","status":"failed","error":null}}}"#,
            &opts(None),
        );
        assert!(
            matches!(&o[0], Out::TurnEnd { status: TurnStatus::Failed, reason: Some(r), .. } if r == "The 'x' model is not supported")
        );
        let (_, o) = handle(
            &mut st,
            r#"{"method":"turn/completed","params":{"threadId":"th","turn":{"id":"t","status":"interrupted"}}}"#,
            &opts(None),
        );
        assert!(matches!(&o[0], Out::TurnEnd { status: TurnStatus::Interrupted, .. }));
    }

    #[test]
    fn approvals_are_accepted_and_questions_are_asked() {
        let mut st = State { thread: Some("th".into()), ready: true, ..Default::default() };
        let (w, _) = handle(
            &mut st,
            r#"{"id":7,"method":"item/commandExecution/requestApproval","params":{"threadId":"th"}}"#,
            &opts(None),
        );
        assert!(w[0].contains("\"accept\""));
        let q = r#"{"id":8,"method":"item/tool/requestUserInput","params":{"threadId":"th","questions":[{"id":"q1","header":"Build","question":"Which config?","options":[{"label":"debug"},{"label":"release"}]}]}}"#;
        let (w, o) = handle(&mut st, q, &opts(None));
        assert!(w.is_empty());
        assert!(matches!(&o[0], Out::Ev(Ev::Question { qid, options, .. }) if qid == "q1" && options.len() == 2));
        assert_eq!(st.asking.len(), 1);
        let (w, _) =
            handle(&mut st, r#"{"id":9,"method":"account/chatgptAuthTokens/refresh","params":{}}"#, &opts(None));
        assert!(w[0].contains("-32601"));
    }

    fn ask(st: &mut State, id: Value, qs: Value) -> Vec<Out> {
        let line = json!({"id": id, "method": "item/tool/requestUserInput",
            "params": {"threadId": "th", "turnId": "t", "itemId": "call-1", "isBlocking": true, "questions": qs}});
        handle(st, &line.to_string(), &opts(None)).1
    }

    #[test]
    fn a_question_says_its_request_header_descriptions_and_whether_other_answers_are_welcome() {
        let mut st = State { thread: Some("th".into()), ready: true, ..Default::default() };
        let o = ask(
            &mut st,
            json!(8),
            json!([
                {"id": "scope", "header": "Scope", "question": "Fix monthly.py too?", "isOther": true,
                 "options": [{"label": "Fix it too (Recommended)", "description": "Same bug"}, {"label": "Only export.py", "description": ""}]},
                {"id": "token", "header": "", "question": "Your token?", "isSecret": true},
            ]),
        );
        let Out::Ev(Ev::Question {
            qid,
            prompt,
            options,
            request,
            item_id,
            header,
            descriptions,
            other,
            secret,
            blocking,
            expires_in_ms,
        }) = &o[0]
        else {
            panic!("{o:?}")
        };
        assert_eq!((qid.as_str(), prompt.as_str()), ("scope", "Fix monthly.py too?"), "the header is its own field");
        assert_eq!(options, &["Fix it too (Recommended)", "Only export.py"]);
        assert_eq!(descriptions, &["Same bug", ""]);
        assert_eq!(
            (request.as_deref(), item_id.as_deref(), header.as_deref()),
            (Some("0-8"), Some("call-1"), Some("Scope")),
            "the request is named by the process's start and its id"
        );
        assert!(*other && !*secret && *blocking == Some(true) && expires_in_ms.is_none());
        let Out::Ev(Ev::Question { header, descriptions, other, secret, options, .. }) = &o[1] else { panic!() };
        assert!(header.is_none() && descriptions.is_empty() && options.is_empty() && !*other && *secret);
    }

    #[test]
    fn answers_go_back_per_request_without_the_skipped_ones() {
        let mut st = State { thread: Some("th".into()), ready: true, ..Default::default() };
        ask(&mut st, json!(8), json!([{"id": "a", "question": "A?"}, {"id": "b", "question": "B?"}]));
        ask(&mut st, json!("req-9"), json!([{"id": "a", "question": "Again A?"}]));
        assert_eq!(st.asking.len(), 2, "a second request does not replace the first");
        // The later request's `a` first, then the earlier one's, skipping its `b`.
        let (reply, done) = answer(&mut st, "a".into(), " yes ".into()).expect("the later request is complete");
        assert!(
            reply.contains(r#""id":"req-9""#) && reply.contains(r#""answers":{"a":{"answers":["yes"]}}"#),
            "{reply}"
        );
        assert!(
            matches!(&done, Ev::QuestionDone { request, how, answers } if request == "0-req-9" && how == "answered" && answers["a"] == ["yes"])
        );
        assert!(answer(&mut st, "a".into(), "first".into()).is_none(), "b is still open");
        let (reply, done) = answer(&mut st, "b".into(), "  ".into()).unwrap();
        assert!(reply.contains(r#""id":8"#) && reply.contains(r#""answers":{"a":{"answers":["first"]}}"#), "{reply}");
        assert!(matches!(&done, Ev::QuestionDone { answers, .. } if !answers.contains_key("b")));
        // Every answer empty: an empty reply, said to be skipped.
        ask(&mut st, json!(10), json!([{"id": "c", "question": "C?"}]));
        let (reply, done) = answer(&mut st, "c".into(), "".into()).unwrap();
        assert!(reply.contains(r#""answers":{}"#), "{reply}");
        assert!(matches!(&done, Ev::QuestionDone { how, .. } if how == "skipped"));
        assert!(st.asking.is_empty() && answer(&mut st, "c".into(), "late".into()).is_none());
    }

    #[test]
    fn questions_codex_closes_or_its_turn_ends_on_are_over() {
        let mut st = State { thread: Some("th".into()), turn: Some("t".into()), ready: true, ..Default::default() };
        ask(&mut st, json!(8), json!([{"id": "a", "question": "A?"}]));
        ask(&mut st, json!(9), json!([{"id": "b", "question": "B?"}]));
        let (_, o) = handle(
            &mut st,
            r#"{"method":"serverRequest/resolved","params":{"threadId":"th","requestId":8}}"#,
            &opts(None),
        );
        assert!(
            matches!(&o[..], [Out::Ev(Ev::QuestionDone { request, how, .. })] if request == "0-8" && how == "resolved"),
            "{o:?}"
        );
        let (_, o) = handle(
            &mut st,
            r#"{"method":"turn/completed","params":{"threadId":"th","turn":{"id":"t","status":"interrupted"}}}"#,
            &opts(None),
        );
        assert!(
            matches!(&o[0], Out::Ev(Ev::QuestionDone { request, how, .. }) if request == "0-9" && how == "dismissed"),
            "{o:?}"
        );
        assert!(matches!(o.last(), Some(Out::TurnEnd { .. })));
        assert!(st.asking.is_empty());
    }

    #[test]
    fn a_question_that_does_not_block_is_answered_with_nothing_once_its_wait_is_over() {
        let mut st =
            State { thread: Some("th".into()), turn: Some("t".into()), ready: true, epoch: 77, ..Default::default() };
        let line = json!({"id": 3, "method": "item/tool/requestUserInput",
            "params": {"threadId": "th", "turnId": "t", "itemId": "call-9", "isBlocking": false,
                "questions": [{"id": "name", "question": "Name?"}]}});
        let o = handle(&mut st, &line.to_string(), &opts(None)).1;
        assert!(
            matches!(&o[0], Out::Ev(Ev::Question { request, expires_in_ms, .. })
                if request.as_deref() == Some("77-3") && *expires_in_ms == Some(QUESTION_WAIT.as_millis() as u64)),
            "{o:?}"
        );
        ask(&mut st, json!(4), json!([{"id": "b", "question": "Blocking?"}]));
        let now = tokio::time::Instant::now();
        assert!(expire(&mut st, now).is_empty(), "nothing is over yet");
        let over = expire(&mut st, now + QUESTION_WAIT + std::time::Duration::from_secs(1));
        assert_eq!(over.len(), 1, "only the question that does not block");
        assert!(over[0].0.contains(r#""id":3"#) && over[0].0.contains(r#""answers":{}"#), "{}", over[0].0);
        assert!(matches!(&over[0].1, Ev::QuestionDone { request, how, .. } if request == "77-3" && how == "expired"));
        assert_eq!(st.asking.len(), 1);
        assert!(st.asking[0].deadline.is_none());
    }

    #[test]
    fn requests_of_two_processes_have_different_names() {
        let mut a = State { thread: Some("th".into()), ready: true, epoch: 1, ..Default::default() };
        let mut b = State { thread: Some("th".into()), ready: true, epoch: 2, ..Default::default() };
        let name = |o: &[Out]| match &o[0] {
            Out::Ev(Ev::Question { request, .. }) => request.clone(),
            _ => None,
        };
        let qa = ask(&mut a, json!(0), json!([{"id": "q", "question": "?"}]));
        let qb = ask(&mut b, json!(0), json!([{"id": "q", "question": "?"}]));
        assert_ne!(name(&qa), name(&qb), "Codex numbers requests from 0 in each process");
    }

    #[test]
    fn only_the_reasoning_summary_shows_one_paragraph_per_part() {
        let mut st = State { thread: Some("th".into()), turn: Some("t".into()), ready: true, ..Default::default() };
        let mut thoughts = String::new();
        for l in [
            r#"{"method":"item/reasoning/summaryPartAdded","params":{"threadId":"th","itemId":"r","summaryIndex":0}}"#,
            r#"{"method":"item/reasoning/summaryTextDelta","params":{"threadId":"th","itemId":"r","delta":"**Reading**","summaryIndex":0}}"#,
            r#"{"method":"item/reasoning/textDelta","params":{"threadId":"th","itemId":"r","delta":"raw chain of thought","contentIndex":0}}"#,
            r#"{"method":"item/reasoning/summaryPartAdded","params":{"threadId":"th","itemId":"r","summaryIndex":1}}"#,
            r#"{"method":"item/reasoning/summaryTextDelta","params":{"threadId":"th","itemId":"r","delta":"**Writing**","summaryIndex":1}}"#,
        ] {
            for o in handle(&mut st, l, &opts(None)).1 {
                if let Out::Ev(Ev::Thought { text, .. }) = o {
                    thoughts.push_str(&text);
                }
            }
        }
        assert_eq!(thoughts, "**Reading**\n\n**Writing**");
        // Not streamed: the parts joined the same way.
        let (_, o) = handle(
            &mut st,
            r#"{"method":"item/completed","params":{"threadId":"th","item":{"type":"reasoning","id":"r2","summary":["**A**","**B**"],"content":["raw"]}}}"#,
            &opts(None),
        );
        assert!(matches!(&o[..], [Out::Ev(Ev::Thought { text, .. })] if text == "**A**\n\n**B**"), "{o:?}");
    }

    #[test]
    fn steers_go_to_the_running_turn_or_wait_for_its_id() {
        let mut st = State { thread: Some("th".into()), ready: true, ..Default::default() };
        // No turn: refused.
        assert!(
            matches!(steer(&mut st, "s0".into(), "x".into(), vec![]), Err(o) if matches!(*o, Out::SteerRejected { ref id, .. } if id == "s0"))
        );
        // Asked for, no id yet: waits, then goes out with the id.
        let _ = turn_start(&mut st, "count", &[]);
        assert!(matches!(steer(&mut st, "s1".into(), "stop at 5".into(), vec![]), Ok(None)));
        let id = st.next;
        let (w, _) = handle(&mut st, &format!(r#"{{"id":{id},"result":{{"turn":{{"id":"tu"}}}}}}"#), &opts(None));
        let req: Value = serde_json::from_str(&w[0]).unwrap();
        assert_eq!(req["method"], "turn/steer");
        assert_eq!(req["params"]["expectedTurnId"], "tu");
        assert_eq!(req["params"]["input"][0]["text"], "stop at 5");
        let (_, o) = handle(&mut st, &format!(r#"{{"id":{},"result":{{"turnId":"tu"}}}}"#, req["id"]), &opts(None));
        assert!(matches!(&o[0], Out::Steered { id } if id == "s1"));
        // The turn ended meanwhile: refused, so it is queued.
        let line = steer(&mut st, "s2".into(), "more".into(), vec![]).unwrap().unwrap();
        let req: Value = serde_json::from_str(&line).unwrap();
        let (_, o) = handle(
            &mut st,
            &format!(r#"{{"id":{},"error":{{"code":-32600,"message":"no active turn to steer"}}}}"#, req["id"]),
            &opts(None),
        );
        assert!(matches!(&o[0], Out::SteerRejected { id, reason } if id == "s2" && reason.contains("no active turn")));
    }

    #[test]
    fn output_streams_diffs_split_per_hunk_and_usage_has_the_context() {
        let mut st = State { thread: Some("th".into()), ready: true, ..Default::default() };
        let diff = "--- a/f.rs\n+++ b/f.rs\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n@@ -10,2 +10,3 @@\n x\n+y\n z\n";
        let lines = [
            r#"{"method":"item/started","params":{"threadId":"th","item":{"type":"commandExecution","id":"c1","command":"ls","status":"inProgress"}}}"#.to_string(),
            r#"{"method":"item/commandExecution/outputDelta","params":{"threadId":"th","itemId":"c1","delta":"1\n"}}"#.to_string(),
            r#"{"method":"item/completed","params":{"threadId":"th","item":{"type":"commandExecution","id":"c1","status":"failed","aggregatedOutput":"1\n","exitCode":3}}}"#.to_string(),
            json!({"method": "item/started", "params": {"threadId": "th", "item": {"type": "fileChange", "id": "f1", "status": "inProgress", "changes": [{"path": "/w/f.rs", "kind": {"type": "update"}, "diff": diff}]}}}).to_string(),
            r#"{"method":"item/completed","params":{"threadId":"th","item":{"type":"fileChange","id":"f1","status":"completed","changes":[]}}}"#.to_string(),
            r#"{"method":"thread/tokenUsage/updated","params":{"threadId":"th","turnId":"t","tokenUsage":{"last":{"inputTokens":90,"outputTokens":10,"totalTokens":100},"total":{},"modelContextWindow":475000}}}"#.to_string(),
            r#"{"method":"item/completed","params":{"threadId":"th","item":{"type":"plan","id":"p1","text":"1. do it"}}}"#.to_string(),
        ];
        let mut outs = Vec::new();
        for l in &lines {
            outs.extend(handle(&mut st, l, &opts(None)).1);
        }
        let shown: Vec<String> = outs.iter().map(|o| format!("{o:?}")).collect();
        assert!(
            matches!(&outs[1], Out::Ev(Ev::ToolUpdate { status: None, content, .. }) if matches!(&content[0], ToolContent::Text { text } if text == "1\n")),
            "{shown:?}"
        );
        // The output is not sent again at the end; the exit code is.
        assert!(
            matches!(&outs[2], Out::Ev(Ev::ToolUpdate { status: Some(ToolStatus::Failed), content, .. }) if matches!(&content[..], [ToolContent::Text { text }] if text == "(exit code 3)")),
            "{shown:?}"
        );
        let Out::Ev(Ev::ToolCall { content, .. }) = &outs[3] else { panic!("{shown:?}") };
        assert_eq!(content.len(), 2, "one diff per hunk");
        assert!(
            matches!(&content[0], ToolContent::Diff { old: Some(o), new, .. } if o == "a\nb\nc\n" && new == "a\nB\nc\n")
        );
        assert!(
            matches!(&content[1], ToolContent::Diff { old: Some(o), new, .. } if o == "x\nz\n" && new == "x\ny\nz\n")
        );
        assert!(matches!(&outs[4], Out::Ev(Ev::ToolUpdate { content, .. }) if content.is_empty()), "{shown:?}");
        assert!(matches!(&outs[5], Out::Ev(Ev::Usage { context_window: Some(475000), context_used: Some(100), .. })));
        assert!(matches!(&outs[6], Out::Ev(Ev::Message { text, delta: false, .. }) if text == "1. do it"));
    }

    #[test]
    fn skills_are_listed_and_a_message_naming_one_carries_it() {
        let mut st = State::default();
        let _ = st.request("initialize", json!({}), Pending::Initialize);
        handle(&mut st, r#"{"id":1,"result":{}}"#, &opts(None));
        let (w, _) = handle(&mut st, r#"{"id":2,"result":{"thread":{"id":"th"}}}"#, &opts(None));
        let list: Value = serde_json::from_str(w.last().unwrap()).unwrap();
        assert_eq!((list["method"].as_str(), list["params"]["cwds"][0].as_str()), (Some("skills/list"), Some("/w")));
        let (_, outs) = handle(
            &mut st,
            r#"{"id":3,"result":{"data":[{"cwd":"/w","errors":[],"skills":[
                {"name":"tidy","description":"Long text","shortDescription":"Tidy up","enabled":true,"path":"/s/tidy/SKILL.md","scope":"user"},
                {"name":"off","description":"x","enabled":false,"path":"/s/off/SKILL.md","scope":"user"}]}]}}"#,
            &opts(None),
        );
        let Some(Out::Ev(Ev::Commands { commands })) = outs.first() else { panic!("{outs:?}") };
        assert_eq!(commands.len(), 1);
        assert_eq!(
            (commands[0].insert.as_str(), commands[0].description.as_str(), commands[0].kind.as_str()),
            ("$tidy", "Tidy up", "skill")
        );
        let start: Value = serde_json::from_str(&turn_start(&mut st, "please $tidy the module", &[])).unwrap();
        assert_eq!(start["params"]["input"][1], json!({"type": "skill", "name": "tidy", "path": "/s/tidy/SKILL.md"}));
        let plain: Value = serde_json::from_str(&turn_start(&mut st, "pay $tidyness or a$tidy", &[])).unwrap();
        assert_eq!(plain["params"]["input"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn images_go_as_data_urls() {
        let mut st = State::default();
        let img = crate::chat::Image::parse("s.jpg", "image/jpeg", "/9j/4AAQ").unwrap();
        let start: Value = serde_json::from_str(&turn_start(&mut st, "look", &[img])).unwrap();
        assert_eq!(start["params"]["input"][1], json!({"type": "image", "url": "data:image/jpeg;base64,/9j/4AAQ"}));
    }
}
