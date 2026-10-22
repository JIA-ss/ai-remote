//! Codex, driven through `codex app-server` (JSON-RPC over stdio): `initialize`, then a thread
//! (`thread/start`, or `thread/resume` to continue one), then one `turn/start` per message on the
//! same process. Measured with codex-cli 0.163: two turns on one thread keep their context,
//! `turn/interrupt` ends a running turn within milliseconds (`turn/completed` with status
//! `interrupted`), and `thread/resume` in a new process continues the conversation.
//!
//! - Bypass: the thread starts with `approvalPolicy: "never"` and `sandbox: "danger-full-access"`;
//!   approval requests that still come are accepted.
//! - `item/tool/requestUserInput` (the agent asks the user) becomes question events; the answers
//!   go back when every question has one.
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

use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{LiveCmd, exit_reason, report};
use crate::chat::{
    Image,
    event::{
        DIFF_SIDE, Ev, Out, PREVIEW, PlanEntry, SlashCommand, ToolContent, ToolKind, ToolStatus, TurnStatus, cut,
        preview_value,
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
    vec!["app-server".into()]
}

/// What a request we sent was for.
#[derive(Debug)]
enum Pending {
    Initialize,
    Thread { resumed: bool },
    TurnStart,
    Steer { id: String },
    Skills,
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
    /// A question request waiting for answers: its JSON-RPC id, the question ids and the answers so far.
    asking: Option<(Value, Vec<String>, HashMap<String, String>)>,
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
    let mut st = State::default();
    let init = st.request(
        "initialize",
        json!({"clientInfo": {"name": "ai-remote", "title": "ai-remote", "version": ar_core::VERSION}, "capabilities": null}),
        Pending::Initialize,
    );
    if proc.write_line(init).await.is_err() {
        report(&out, Out::Exited(exit_reason(&name, &proc, Some("could not start".into())))).await;
        return;
    }
    loop {
        tokio::select! {
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
                    let mut reply = None;
                    if let Some((id, qids, answers)) = st.asking.as_mut() {
                        if qids.contains(&qid) {
                            answers.insert(qid, text);
                        }
                        if qids.iter().all(|q| answers.contains_key(q)) {
                            let map: serde_json::Map<String, Value> =
                                answers.iter().map(|(k, v)| (k.clone(), json!({"answers": [v]}))).collect();
                            reply = Some(jsonrpc::response(id, json!({"answers": map})));
                        }
                    }
                    if let Some(r) = reply {
                        st.asking = None;
                        let _ = proc.write_line(r).await;
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
                    let mut qids = Vec::new();
                    for q in params["questions"].as_array().into_iter().flatten() {
                        let qid = q["id"].as_str().unwrap_or("").to_string();
                        let header = q["header"].as_str().unwrap_or("");
                        let question = q["question"].as_str().unwrap_or("");
                        let prompt =
                            if header.is_empty() { question.to_string() } else { format!("{header}: {question}") };
                        let options = q["options"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|o| o["label"].as_str().map(str::to_string))
                            .collect();
                        qids.push(qid.clone());
                        outs.push(Out::Ev(Ev::Question { qid, prompt, options }));
                    }
                    if qids.is_empty() {
                        writes.push(jsonrpc::response(&id, json!({"answers": {}})));
                    } else {
                        st.asking = Some((id, qids, HashMap::new()));
                    }
                }
                other => writes.push(jsonrpc::unsupported(&id, other)),
            }
        }
        Incoming::Notification { method, params } => {
            if params["threadId"].as_str().is_some_and(|t| Some(t) != st.thread.as_deref()) {
                return (writes, outs);
            }
            notification(st, &method, &params, &mut outs);
        }
    }
    (writes, outs)
}

fn notification(st: &mut State, method: &str, p: &Value, outs: &mut Vec<Out>) {
    match method {
        "item/agentMessage/delta" => {
            let id = p["itemId"].as_str().unwrap_or("").to_string();
            st.streamed.insert(id.clone());
            outs.push(Out::Ev(Ev::Message { id, text: p["delta"].as_str().unwrap_or("").into(), delta: true }));
        }
        "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
            let id = p["itemId"].as_str().unwrap_or("").to_string();
            st.streamed.insert(id.clone());
            outs.push(Out::Ev(Ev::Thought { id, text: p["delta"].as_str().unwrap_or("").into(), delta: true }));
        }
        "item/started" => {
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
            }));
        }
        "item/fileChange/patchUpdated" => {
            let id = p["itemId"].as_str().unwrap_or("").to_string();
            let content = diffs(&p["changes"]);
            if !content.is_empty() && st.diffed.insert(id.clone()) {
                outs.push(Out::Ev(Ev::ToolUpdate { call_id: id, status: None, content, truncated: false }));
            }
        }
        "item/mcpToolCall/progress" => {
            if let Some(m) = p["message"].as_str().filter(|m| !m.is_empty()) {
                outs.push(Out::Ev(Ev::ToolUpdate {
                    call_id: p["itemId"].as_str().unwrap_or("").to_string(),
                    status: None,
                    content: vec![ToolContent::Text { text: format!("{}\n", cut(m, 400).0) }],
                    truncated: false,
                }));
            }
        }
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
        "error" => {
            let m = p["error"]["message"].as_str().unwrap_or("error");
            // The message is often the provider's JSON; its inner message reads better.
            let inner = serde_json::from_str::<Value>(m)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or_else(|| m.to_string());
            st.last_error = Some(inner);
        }
        "turn/completed" => {
            let turn = &p["turn"];
            st.turn = None;
            st.starting = false;
            st.output.clear();
            st.diffed.clear();
            let status = match turn["status"].as_str().unwrap_or("") {
                "completed" => TurnStatus::Completed,
                "interrupted" => TurnStatus::Interrupted,
                _ => TurnStatus::Failed,
            };
            let reason = turn["error"]["message"].as_str().map(str::to_string).or_else(|| st.last_error.take());
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
            (ToolKind::Execute, item["command"].as_str().unwrap_or("command").to_string(), vec![], None)
        }
        "fileChange" => {
            let paths: Vec<&str> =
                item["changes"].as_array().into_iter().flatten().filter_map(|c| c["path"].as_str()).collect();
            (ToolKind::Edit, format!("edit {}", paths.join(", ")), diffs(&item["changes"]), None)
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
            if !st.streamed.contains(&id) && !text.is_empty() {
                outs.push(Out::Ev(Ev::Message { id, text: text.clone(), delta: false }));
            }
            if item["phase"].as_str() != Some("commentary") {
                st.final_text = Some(text);
            }
        }
        "reasoning" => {
            let text: Vec<&str> = item["summary"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            if !st.streamed.contains(&id) && !text.is_empty() {
                outs.push(Out::Ev(Ev::Thought { id, text: text.join("\n"), delta: false }));
            }
        }
        "plan" => {
            // The plan Codex proposes in plan mode (its deltas may not add up to it, so only the
            // complete text is shown).
            let text = item["text"].as_str().unwrap_or("");
            if !text.is_empty() {
                outs.push(Out::Ev(Ev::Message { id, text: text.to_string(), delta: false }));
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
            outs.push(Out::Ev(Ev::ToolUpdate { call_id: id, status: Some(status), content, truncated }));
        }
        _ => {}
    }
}

/// The diffs of a file change, one per hunk (old and new text of the hunk), so they show as
/// diffs with the right counts of added and removed lines.
fn diffs(changes: &Value) -> Vec<ToolContent> {
    let mut out = Vec::new();
    for c in changes.as_array().into_iter().flatten() {
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
            out.push(ToolContent::Diff { path, old: old.map(|o| cut(&o, DIFF_SIDE).0), new: cut(&new, DIFF_SIDE).0 });
        } else {
            for (old, new) in hunks {
                out.push(ToolContent::Diff {
                    path: path.clone(),
                    old: (kind != "add").then(|| cut(&old, DIFF_SIDE).0),
                    new: cut(&new, DIFF_SIDE).0,
                });
            }
        }
        if out.len() >= 40 {
            break;
        }
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
        assert_eq!(outs.len(), 4, "{shown:?}");
        assert!(matches!(&outs[1], Out::Ev(Ev::ToolCall { kind: ToolKind::Execute, .. })));
        assert!(
            matches!(&outs[2], Out::Ev(Ev::ToolUpdate { status: Some(ToolStatus::Failed), content, .. }) if matches!(&content[0], ToolContent::Text { text } if text.contains("exit code 2")))
        );
        assert!(
            matches!(&outs[3], Out::TurnEnd { status: TurnStatus::Completed, final_text: Some(t), .. } if t == "ONE")
        );
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
        assert!(st.asking.is_some());
        let (w, _) =
            handle(&mut st, r#"{"id":9,"method":"account/chatgptAuthTokens/refresh","params":{}}"#, &opts(None));
        assert!(w[0].contains("-32601"));
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
