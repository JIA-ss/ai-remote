//! Claude Code, driven as `claude -p --input-format stream-json --output-format stream-json`: one
//! JSON line per user message in, one per event out. The process stays up between turns (each
//! user line starts a turn that ends with a `result` line), so a conversation keeps its context
//! without re-reading the transcript; `--resume <session>` continues it in a new process.
//!
//! - Permissions: `--permission-mode <the session's mode>` (`bypassPermissions` unless the session
//!   chose another) and `--permission-prompt-tool stdio`, so whatever the mode says to ask about
//!   comes as a `control_request` `can_use_tool` (measured with Claude Code 2.1.308, see
//!   docs/agent-workspace-notes/host.md):
//!   - `AskUserQuestion` comes in every mode, bypass included: it becomes a `question` event with
//!     every part; the answer is allow with `updatedInput = {questions, answers}`, which the model
//!     reads as the person's answers.
//!   - `ExitPlanMode` (plan mode) comes with the plan in its input: a `plan_review` event; approved,
//!     it is allowed and the mode is switched (`set_permission_mode`); not approved, it is denied
//!     with what to change.
//!   - Any other tool: in bypass mode it is allowed at once; otherwise a `permission` event. Allowed
//!     for the session, the agent's own `permission_suggestions` go back as `updatedPermissions`
//!     with their destination set to `session` (as suggested they would be written to the
//!     project's `.claude/settings.local.json`). Every allow turns `run_in_background` off: a
//!     background shell outlives the turn that started it, and nothing here would report it.
//!   - Denied with a message, the model reads the message and goes on (`interrupt: false`);
//!     without one the turn stops. An interrupt denies what is still asked.
//! - The mode: `system` lines (`init`, `status`) carry `permissionMode`; each change is a `mode`
//!   event. `set_permission_mode` answers `{mode}` or an error (`mode_rejected`).
//! - Title: `generate_session_title` with the first message answers `{title}` (a `title` event).
//! - Text streams with `--include-partial-messages`; the complete `assistant` message that follows
//!   only contributes what did not stream (its tool calls).
//! - Interrupt: a `control_request` of subtype `interrupt`; the turn then ends with a `result`.
//! - A turn ends with its `result` line only. A `result` that reports an error (401, a prompt too
//!   long) ends the turn as failed with its text.
//! - Steer: a user line written while a turn runs is read at the turn's next model request (after
//!   the tool results it is waiting for) and becomes part of that turn; written after the turn's
//!   last request, it starts a turn of its own once the `result` is out (a new `system` `init`,
//!   then its own `result`). `--replay-user-messages` echoes every user line when the agent takes
//!   it (`isReplay: true`), which tells the two apart: echoed before the `result`, the message
//!   joined the turn ([`Out::Steered`]); still unechoed at the `result`, it runs next
//!   ([`Out::SteerLate`]). Measured with Claude Code 2.1.293, see AGENTS.md.
//! - Commands: a `control_request` of subtype `initialize`, written at start, is answered with
//!   every slash command and skill with its description (`commands`), before any message; the
//!   first turn's `system` `init` then says which of them are skills (`skills`). Agents too old to
//!   answer it still list the names in `init` (`slash_commands`).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::chat::{Image, event::SlashCommand, random_hex};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{DEFAULT_PERMISSION_MODE, Decision, LiveCmd, PERMISSION_MODES, content_text, exit_reason, report};
use crate::chat::{
    event::{
        DIFF_SIDE, Ev, Hunk, MAX_HUNKS, Out, Outcome, PREVIEW, PlanEntry, QuestionItem, QuestionOption, ToolContent,
        ToolKind, ToolStatus, TurnStatus, cut, preview_value,
    },
    proc::{AgentProc, Line},
};

/// The mode a session asked for, if Claude Code knows it; the default otherwise.
pub fn mode_or_default(mode: Option<&str>) -> &str {
    mode.filter(|m| PERMISSION_MODES.contains(m)).unwrap_or(DEFAULT_PERMISSION_MODE)
}

pub fn args(l: &super::Launch<'_>) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--replay-user-messages",
        "--permission-prompt-tool",
        "stdio",
        "--permission-mode",
        mode_or_default(l.permission_mode),
    ]
    .map(String::from)
    .to_vec();
    if l.hardened {
        a.extend(["--setting-sources", "user", "--strict-mcp-config"].map(String::from));
    }
    if let Some(m) = l.model {
        a.extend(["--model".into(), m.into()]);
    }
    if let Some(r) = l.resume {
        a.extend(["--resume".into(), r.into()]);
    }
    a
}

#[derive(Default)]
struct State {
    session: Option<String>,
    /// The message being streamed and the ids of messages whose text already streamed.
    current: Option<String>,
    streamed: HashSet<String>,
    /// Tool calls reported, so results of unknown ones (a plan update) are not shown.
    calls: HashSet<String>,
    interrupting: bool,
    retried: bool,
    next_req: u64,
    /// User lines written and not yet echoed, oldest first; `Some` for steers.
    written: VecDeque<Written>,
    /// A user line of the running turn was echoed (the turn's own message, or a steer).
    echoed: bool,
    /// The last model request of the turn: its model and how many tokens it took in and out.
    last_model: Option<String>,
    last_context: Option<u64>,
    /// The commands `initialize` listed (name, description, argument hint), and which are skills.
    commands: Vec<(String, String, String)>,
    skills: HashSet<String>,
    /// An `init` said which commands are skills: the list goes out from then on (before that, every
    /// start would send it twice, once without its skills).
    skills_known: bool,
    /// The permission mode: as started until the agent says, then as it last said.
    mode: String,
    /// A `mode` event went out for `mode`.
    mode_said: bool,
    /// What the agent asks and waits on a person for, by the id the events carry.
    asks: HashMap<String, Ask>,
}

/// A `can_use_tool` request waiting for a person.
struct Ask {
    /// Claude Code's id of the request, for the answer.
    request: Value,
    tool: String,
    input: Value,
    /// The agent's `permission_suggestions` (rules or a mode to allow it for the session).
    suggestions: Vec<Value>,
}

impl State {
    fn new(mode: &str) -> Self {
        Self { mode: mode_or_default(Some(mode)).to_string(), ..Default::default() }
    }

    fn request_id(&mut self, what: &str) -> String {
        self.next_req += 1;
        format!("ar-{what}-{}", self.next_req)
    }
}

/// Most tool calls of a session followed (results of others are not shown).
const MAX_CALLS: usize = 100_000;

/// The request id of the `initialize` written at start.
const INIT_REQUEST: &str = "ar-init";

/// The `initialize` request that lists the agent's commands.
pub fn initialize_line() -> String {
    json!({"type": "control_request", "request_id": INIT_REQUEST, "request": {"subtype": "initialize"}}).to_string()
}

impl State {
    fn command_list(&self) -> Out {
        let commands = self
            .commands
            .iter()
            .map(|(name, description, hint)| {
                let kind = if self.skills.contains(name) { "skill" } else { "command" };
                SlashCommand::new(kind, name, format!("/{name}"), description, Some(hint.as_str()))
            })
            .collect();
        Out::Ev(Ev::Commands { commands })
    }
}

struct Written {
    steer: Option<String>,
    /// Reported as [`Out::SteerLate`]: the agent runs it as a turn of its own.
    late: bool,
}

pub async fn run(
    proc: AgentProc,
    mut lines: mpsc::Receiver<Line>,
    mut cmds: mpsc::Receiver<LiveCmd>,
    out: mpsc::Sender<Out>,
    name: String,
    mode: String,
) {
    let mut st = State::new(&mode);
    let _ = proc.write_line(initialize_line()).await;
    loop {
        tokio::select! {
            line = lines.recv() => match line {
                Some(Ok(line)) => {
                    let (writes, outs) = handle(&mut st, &line);
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
                    st.retried = false;
                    match proc.write_line(user_line(&text, &images)).await {
                        Ok(()) => st.written.push_back(Written { steer: None, late: false }),
                        Err(e) => {
                            report(&out, Out::TurnEnd { status: TurnStatus::Failed, final_text: None, reason: Some(format!("{e:#}")) }).await;
                        }
                    }
                }
                Some(LiveCmd::Steer { id, text, images }) => match proc.write_line(user_line(&text, &images)).await {
                    Ok(()) => st.written.push_back(Written { steer: Some(id), late: false }),
                    Err(e) => {
                        report(&out, Out::SteerRejected { id, reason: format!("{e:#}") }).await;
                    }
                },
                Some(cmd @ (LiveCmd::Interrupt | LiveCmd::Answer { .. } | LiveCmd::Permit { .. } | LiveCmd::SetMode { .. } | LiveCmd::GenerateTitle { .. })) => {
                    let (writes, outs) = command(&mut st, cmd);
                    for w in writes {
                        let _ = proc.write_line(w).await;
                    }
                    for o in outs {
                        report(&out, o).await;
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

/// A user message: its images (as base64 image blocks, the way the Messages API takes them),
/// then its text.
fn user_line(text: &str, images: &[Image]) -> String {
    let mut content: Vec<Value> = images
        .iter()
        .map(|i| json!({"type": "image", "source": {"type": "base64", "media_type": i.mime, "data": &*i.data}}))
        .collect();
    if !text.trim().is_empty() || content.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    json!({"type": "user", "message": {"role": "user", "content": content}}).to_string()
}

fn control(request_id: String, request: Value) -> String {
    json!({"type": "control_request", "request_id": request_id, "request": request}).to_string()
}

fn reply(request: &Value, response: Value) -> String {
    json!({"type": "control_response", "response": {"subtype": "success", "request_id": request, "response": response}})
        .to_string()
}

/// Allowing a tool as asked, with `run_in_background` turned off (see the module notes).
fn allow(mut input: Value) -> Value {
    if input.get("run_in_background").is_some() {
        input["run_in_background"] = json!(false);
    }
    json!({"behavior": "allow", "updatedInput": input})
}

/// Not allowing it: with a message the model reads it and goes on, without one the turn stops.
fn deny(message: Option<&str>) -> Value {
    match message.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => json!({"behavior": "deny", "message": m, "interrupt": false}),
        None => json!({"behavior": "deny", "message": "The user declined this.", "interrupt": true}),
    }
}

fn notice(code: &'static str, text: String) -> Out {
    Out::Ev(Ev::Notice { input_id: None, level: "warn", code, text })
}

/// Applies a session command that needs no process call of its own; returns lines to write and
/// messages for the session.
fn command(st: &mut State, cmd: LiveCmd) -> (Vec<String>, Vec<Out>) {
    let mut writes = Vec::new();
    let mut outs = Vec::new();
    match cmd {
        LiveCmd::Interrupt => {
            st.interrupting = true;
            // What the turn still asks about would keep it waiting: no, and its cards close.
            for (id, ask) in std::mem::take(&mut st.asks) {
                writes.push(reply(&ask.request, deny(None)));
                outs.push(Out::Ev(Ev::Resolved { id, outcome: Outcome::Cancelled, by: None }));
            }
            let id = st.request_id("int");
            writes.push(control(id, json!({"subtype": "interrupt"})));
        }
        LiveCmd::Answer { qid, text, answers } => match st.asks.get(&qid) {
            Some(ask) if ask.tool == "AskUserQuestion" => {
                let ask = st.asks.remove(&qid).expect("present");
                let questions = ask.input["questions"].clone();
                let answers: BTreeMap<String, String> = match answers.filter(|a| !a.is_empty()) {
                    Some(a) => a,
                    None => questions[0]["question"].as_str().map(|q| (q.to_string(), text)).into_iter().collect(),
                };
                writes.push(reply(
                    &ask.request,
                    json!({"behavior": "allow", "updatedInput": {"questions": questions, "answers": answers}}),
                ));
            }
            _ => outs.push(notice("no_question", "Claude Code is not waiting for that answer".into())),
        },
        LiveCmd::Permit { rid, decision, message, mode } => {
            let Some(ask) = st.asks.remove(&rid) else {
                outs.push(notice("permission_expired", "Claude Code no longer waits for that answer".into()));
                return (writes, outs);
            };
            let response = match (ask.tool.as_str(), decision) {
                (_, Decision::Deny) => deny(message.as_deref()),
                ("ExitPlanMode", _) => {
                    // The plan goes ahead in the mode chosen with it.
                    let mode = mode.filter(|m| PERMISSION_MODES.contains(&m.as_str()) && m != "plan");
                    let id = st.request_id("mode");
                    let mode = mode.unwrap_or_else(|| "default".into());
                    writes.push(reply(&ask.request, allow(ask.input)));
                    writes.push(control(id, json!({"subtype": "set_permission_mode", "mode": mode})));
                    return (writes, outs);
                }
                (_, Decision::Once) => allow(ask.input),
                (_, Decision::Session) => {
                    let mut r = allow(ask.input);
                    // Kept for this session only: as suggested they would be written to the project.
                    let rules: Vec<Value> = ask
                        .suggestions
                        .into_iter()
                        .map(|mut s| {
                            s["destination"] = json!("session");
                            s
                        })
                        .collect();
                    if !rules.is_empty() {
                        r["updatedPermissions"] = json!(rules);
                    }
                    r
                }
            };
            writes.push(reply(&ask.request, response));
        }
        LiveCmd::SetMode { mode } => {
            if PERMISSION_MODES.contains(&mode.as_str()) {
                let id = st.request_id("mode");
                writes.push(control(id, json!({"subtype": "set_permission_mode", "mode": mode})));
            } else {
                outs.push(notice("mode_rejected", format!("'{mode}' is not a permission mode")));
            }
        }
        LiveCmd::GenerateTitle { description } => {
            let id = st.request_id("title");
            let description = cut(&description, 2000).0;
            writes.push(control(
                id,
                json!({"subtype": "generate_session_title", "description": description, "persist": false}),
            ));
        }
        LiveCmd::Prompt { .. } | LiveCmd::Steer { .. } | LiveCmd::Shutdown { .. } => {}
    }
    (writes, outs)
}

/// The parts of an AskUserQuestion input.
fn question_items(input: &Value) -> Vec<QuestionItem> {
    input["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .take(8)
        .filter_map(|q| {
            let text = |v: &Value| v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(|s| cut(s, 2000).0);
            Some(QuestionItem {
                header: text(&q["header"]),
                question: text(&q["question"])?,
                options: q["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(16)
                    .filter_map(|o| {
                        Some(QuestionOption { label: text(&o["label"])?, description: text(&o["description"]) })
                    })
                    .collect(),
                multi: q["multiSelect"].as_bool() == Some(true),
            })
        })
        .collect()
}

/// A `can_use_tool` request: answered at once (bypass mode), or reported for a person to answer.
fn can_use_tool(st: &mut State, request: Value, req: &Value, writes: &mut Vec<String>, outs: &mut Vec<Out>) {
    let tool = req["tool_name"].as_str().unwrap_or("").to_string();
    let input = req["input"].clone();
    let ask = |input: Value| Ask {
        request: request.clone(),
        tool: tool.clone(),
        input,
        suggestions: req["permission_suggestions"].as_array().cloned().unwrap_or_default(),
    };
    match tool.as_str() {
        "AskUserQuestion" => {
            let questions = question_items(&input);
            let Some(first) = questions.first() else {
                writes.push(reply(&request, deny(Some("There was no question to ask."))));
                return;
            };
            let qid = format!("q{}", &random_hex()[..10]);
            outs.push(Out::Ev(Ev::Question {
                qid: qid.clone(),
                prompt: first.question.clone(),
                options: first.options.iter().map(|o| o.label.clone()).collect(),
                questions: questions.clone(),
                request: None,
                item_id: None,
                header: None,
                descriptions: Vec::new(),
                other: false,
                secret: false,
                blocking: None,
                expires_in_ms: None,
            }));
            st.asks.insert(qid, ask(input));
        }
        "ExitPlanMode" => {
            let rid = format!("p{}", &random_hex()[..10]);
            let plan = cut(input["plan"].as_str().unwrap_or(""), DIFF_SIDE).0;
            outs.push(Out::Ev(Ev::PlanReview { rid: rid.clone(), plan }));
            st.asks.insert(rid, ask(input));
        }
        _ if st.mode == "bypassPermissions" => writes.push(reply(&request, allow(input))),
        _ => {
            let rid = format!("p{}", &random_hex()[..10]);
            let (kind, title) = describe(&tool, &input);
            let reason = [&req["decision_reason"], &req["description"]]
                .into_iter()
                .find_map(|v| v.as_str().filter(|s| !s.trim().is_empty()))
                .map(|s| cut(s, 300).0);
            let suggestions = if req["permission_suggestions"].as_array().is_some_and(|s| !s.is_empty()) {
                vec!["session".into()]
            } else {
                vec![]
            };
            outs.push(Out::Ev(Ev::Permission {
                rid: rid.clone(),
                tool: tool.clone(),
                kind: Some(kind),
                title: Some(title),
                input: Some(preview_value(&input)),
                reason,
                suggestions,
                call_id: req["tool_use_id"].as_str().map(str::to_string),
            }));
            st.asks.insert(rid, ask(input));
        }
    }
}

/// The mode Claude Code reports, as a `mode` event when it changed (or was never said).
fn mode_said(st: &mut State, mode: Option<&str>, outs: &mut Vec<Out>) {
    let Some(mode) = mode.filter(|m| !m.is_empty()) else { return };
    if mode != st.mode || !st.mode_said {
        st.mode = mode.to_string();
        st.mode_said = true;
        outs.push(Out::Ev(Ev::Mode { mode: mode.to_string() }));
    }
}

/// The diff of a finished edit with its line numbers, from the result's `structuredPatch`.
fn patched(result: &Value) -> Option<ToolContent> {
    let patch = result["structuredPatch"].as_array().filter(|p| !p.is_empty())?;
    let num = |h: &Value, k: &str| h[k].as_u64().unwrap_or(0);
    let hunks = patch
        .iter()
        .take(MAX_HUNKS)
        .map(|h| {
            Hunk {
                old_start: num(h, "oldStart"),
                old_lines: num(h, "oldLines"),
                new_start: num(h, "newStart"),
                new_lines: num(h, "newLines"),
                lines: h["lines"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| l.as_str().map(str::to_string))
                    .collect(),
            }
            .bounded()
        })
        .collect();
    let text = |k: &str| result[k].as_str();
    Some(ToolContent::Diff {
        path: text("filePath").unwrap_or("").to_string(),
        old: text("oldString").or(text("originalFile")).map(|s| cut(s, DIFF_SIDE).0),
        new: cut(text("newString").or(text("content")).unwrap_or(""), DIFF_SIDE).0,
        hunks,
    })
}

/// Translates one line of Claude Code's output; returns lines to write back and messages for the
/// session.
fn handle(st: &mut State, line: &str) -> (Vec<String>, Vec<Out>) {
    let mut writes = Vec::new();
    let mut outs = Vec::new();
    let Ok(v) = serde_json::from_str::<Value>(line) else { return (writes, outs) };
    // Events of sub-agents (the Agent / Task tool) carry their parent's tool id: their tool calls
    // are shown inside the sub-agent's row (with `parent`), their streamed text is not.
    let nested = v.get("parent_tool_use_id").is_some_and(|p| !p.is_null());
    match v["type"].as_str().unwrap_or("") {
        "system" => match v["subtype"].as_str().unwrap_or("") {
            "init" => {
                if let Some(id) = v["session_id"].as_str()
                    && st.session.as_deref() != Some(id)
                {
                    st.session = Some(id.to_string());
                    outs.push(Out::Session(id.to_string()));
                }
                let skills: HashSet<String> = v["skills"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect();
                let first = !std::mem::replace(&mut st.skills_known, true);
                if st.commands.is_empty() {
                    // No answer to `initialize`: the names alone.
                    st.commands = v["slash_commands"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|n| n.as_str().map(|n| (n.to_string(), String::new(), String::new())))
                        .collect();
                    st.skills = skills;
                    if !st.commands.is_empty() {
                        outs.push(st.command_list());
                    }
                } else if first || skills != st.skills {
                    st.skills = skills;
                    outs.push(st.command_list());
                }
                mode_said(st, v["permissionMode"].as_str(), &mut outs);
            }
            "status" => mode_said(st, v["permissionMode"].as_str(), &mut outs),
            "api_retry" if !st.retried => {
                st.retried = true;
                let err = v["error"].as_str().unwrap_or("error");
                let status = v["error_status"].as_u64().map(|s| format!(" ({s})")).unwrap_or_default();
                outs.push(Out::Ev(Ev::Notice {
                    input_id: None,
                    level: "warn",
                    code: "api_retry",
                    text: format!("the model API failed{status}: {err}; retrying"),
                }));
            }
            _ => {}
        },
        "stream_event" if !nested => {
            let e = &v["event"];
            match e["type"].as_str().unwrap_or("") {
                "message_start" => st.current = e["message"]["id"].as_str().map(str::to_string),
                "content_block_delta" => {
                    let Some(msg) = st.current.clone() else { return (writes, outs) };
                    let id = format!("{msg}:{}", e["index"].as_u64().unwrap_or(0));
                    let d = &e["delta"];
                    match d["type"].as_str().unwrap_or("") {
                        "text_delta" => {
                            st.streamed.insert(msg);
                            outs.push(Out::Ev(Ev::Message {
                                id,
                                text: d["text"].as_str().unwrap_or("").into(),
                                delta: true,
                                phase: None,
                            }));
                        }
                        "thinking_delta" => {
                            st.streamed.insert(msg);
                            outs.push(Out::Ev(Ev::Thought {
                                id,
                                text: d["thinking"].as_str().unwrap_or("").into(),
                                delta: true,
                            }));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        "assistant" if !nested => {
            let u = &v["message"]["usage"];
            if let Some(input) = u["input_tokens"].as_u64() {
                st.last_context = Some(
                    input
                        + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
                        + u["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                        + u["output_tokens"].as_u64().unwrap_or(0),
                );
                st.last_model = v["message"]["model"].as_str().map(str::to_string);
            }
            let msg = v["message"]["id"].as_str().unwrap_or("").to_string();
            let streamed = st.streamed.contains(&msg);
            for (i, block) in v["message"]["content"].as_array().into_iter().flatten().enumerate() {
                let id = format!("{msg}:{i}");
                match block["type"].as_str().unwrap_or("") {
                    "text" if !streamed => outs.push(Out::Ev(Ev::Message {
                        id,
                        text: block["text"].as_str().unwrap_or("").into(),
                        delta: false,
                        phase: None,
                    })),
                    "thinking" if !streamed => outs.push(Out::Ev(Ev::Thought {
                        id,
                        text: block["thinking"].as_str().unwrap_or("").into(),
                        delta: false,
                    })),
                    "tool_use" => {
                        if let Some(o) = tool_call(st, block, None) {
                            outs.push(o);
                        }
                    }
                    _ => {}
                }
            }
        }
        "assistant" => {
            // A sub-agent's own tool calls, shown inside its row; its text stays in its summary.
            let parent = v["parent_tool_use_id"].as_str();
            for block in v["message"]["content"].as_array().into_iter().flatten() {
                if block["type"] == "tool_use"
                    && st.calls.len() < MAX_CALLS
                    && let Some(o) = tool_call(st, block, parent)
                {
                    outs.push(o);
                }
            }
        }
        "user" if !nested && v["isReplay"].as_bool() == Some(true) => {
            // The agent took a user line we wrote.
            if let Some(w) = st.written.pop_front()
                && let Some(id) = w.steer
                && (!w.late || st.echoed)
            {
                // Taken into the running turn (a late one that opens its own turn is that turn's
                // message, already reported).
                outs.push(Out::Steered { id });
            }
            st.echoed = true;
        }
        "user" if v["isReplay"].as_bool() != Some(true) => {
            let results: Vec<&Value> = v["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|b| b["type"] == "tool_result")
                .collect();
            // The message's `tool_use_result` is the result of its one tool call: an edit's diff
            // with line numbers.
            let patch = if results.len() == 1 { patched(&v["tool_use_result"]) } else { None };
            for block in results {
                let call_id = block["tool_use_id"].as_str().unwrap_or("").to_string();
                if !st.calls.contains(&call_id) {
                    continue;
                }
                let (text, truncated) = cut(&content_text(&block["content"]), PREVIEW);
                let failed = block["is_error"].as_bool() == Some(true);
                let mut content: Vec<ToolContent> = patch.clone().into_iter().collect();
                if !text.is_empty() {
                    content.push(ToolContent::Text { text });
                }
                outs.push(Out::Ev(Ev::ToolUpdate {
                    call_id,
                    status: Some(if failed { ToolStatus::Failed } else { ToolStatus::Completed }),
                    content,
                    truncated,
                    exit_code: None,
                    duration_ms: None,
                }));
            }
        }
        "result" => {
            let usage = &v["usage"];
            outs.push(Out::Ev(Ev::Usage {
                input_tokens: usage["input_tokens"].as_u64().map(|i| {
                    i + usage["cache_read_input_tokens"].as_u64().unwrap_or(0)
                        + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                }),
                output_tokens: usage["output_tokens"].as_u64(),
                cost_usd: v["total_cost_usd"].as_f64(),
                context_window: context_window(&v["modelUsage"], st.last_model.as_deref()),
                context_used: st.last_context.take(),
                cache_read_tokens: usage["cache_read_input_tokens"].as_u64(),
                cache_write_tokens: usage["cache_creation_input_tokens"].as_u64(),
                reasoning_tokens: None,
            }));
            // Steers the turn did not take: the agent runs them next, on its own.
            for w in st.written.iter_mut().filter(|w| !w.late) {
                if let Some(id) = &w.steer {
                    w.late = true;
                    outs.push(Out::SteerLate { id: id.clone() });
                }
            }
            st.echoed = false;
            let text = v["result"].as_str().map(str::to_string);
            let failed = v["is_error"].as_bool() == Some(true) || v["subtype"].as_str().is_some_and(|s| s != "success");
            let status = if st.interrupting {
                TurnStatus::Interrupted
            } else if failed {
                TurnStatus::Failed
            } else {
                TurnStatus::Completed
            };
            let reason = (failed && !st.interrupting)
                .then(|| text.clone().unwrap_or_else(|| v["subtype"].as_str().unwrap_or("error").replace('_', " ")));
            st.interrupting = false;
            st.streamed.clear();
            // Nothing is asked once the turn is over.
            for id in std::mem::take(&mut st.asks).into_keys() {
                outs.push(Out::Ev(Ev::Resolved { id, outcome: Outcome::Cancelled, by: None }));
            }
            outs.push(Out::TurnEnd { status, final_text: if failed { None } else { text }, reason });
        }
        "control_response" if v["response"]["request_id"].as_str().is_some_and(|r| r.starts_with("ar-mode-")) => {
            let r = &v["response"];
            if r["subtype"] == "success" {
                mode_said(st, r["response"]["mode"].as_str(), &mut outs);
            } else {
                let why = r["error"].as_str().unwrap_or("refused");
                outs.push(notice("mode_rejected", format!("Claude Code did not switch the permission mode: {why}")));
            }
        }
        "control_response" if v["response"]["request_id"].as_str().is_some_and(|r| r.starts_with("ar-title-")) => {
            if let Some(title) = v["response"]["response"]["title"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
                outs.push(Out::Ev(Ev::Title { title: cut(title, 200).0, source: "agent".into() }));
            }
        }
        "control_cancel_request" => {
            // The agent no longer waits for that answer.
            let gone: Vec<String> =
                st.asks.iter().filter(|(_, a)| a.request == v["request_id"]).map(|(id, _)| id.clone()).collect();
            for id in gone {
                st.asks.remove(&id);
                outs.push(Out::Ev(Ev::Resolved { id, outcome: Outcome::Cancelled, by: None }));
            }
        }
        "control_response" if v["response"]["request_id"] == INIT_REQUEST => {
            let r = &v["response"]["response"];
            st.commands = r["commands"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| {
                    let s = |k: &str| c[k].as_str().unwrap_or("").to_string();
                    c["name"].as_str().map(|n| (n.to_string(), s("description"), s("argumentHint")))
                })
                .collect();
            if st.skills_known && !st.commands.is_empty() {
                outs.push(st.command_list());
            }
        }
        "control_request" => {
            let id = v["request_id"].clone();
            let req = &v["request"];
            if req["subtype"] == "can_use_tool" {
                can_use_tool(st, id, req, &mut writes, &mut outs);
            } else {
                let reply = json!({"type": "control_response", "response": {"subtype": "error", "request_id": id, "error": "not supported by this client"}});
                writes.push(reply.to_string());
            }
        }
        _ => {}
    }
    (writes, outs)
}

/// The context window of `model` in a `result`'s `modelUsage` (keyed by model, sometimes with a
/// suffix such as `[1m]`), or the largest one listed.
fn context_window(usage: &Value, model: Option<&str>) -> Option<u64> {
    let models = usage.as_object()?;
    let of = |v: &Value| v["contextWindow"].as_u64();
    model
        .and_then(|m| {
            models
                .iter()
                .find(|(k, v)| k.as_str() == m || v["canonicalModel"].as_str() == Some(m) || k.starts_with(m))
                .and_then(|(_, v)| of(v))
        })
        .or_else(|| models.values().filter_map(of).max())
}

/// What a tool does and the line that names it (`Bash cargo build`).
fn describe(name: &str, input: &Value) -> (ToolKind, String) {
    let s = |k: &str| input[k].as_str().unwrap_or("").to_string();
    let (kind, what) = match name {
        "Bash" => (ToolKind::Execute, s("command")),
        "Read" | "NotebookRead" => (ToolKind::Read, s("file_path")),
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => (ToolKind::Edit, s("file_path")),
        "Grep" => (ToolKind::Search, s("pattern")),
        "Glob" => (ToolKind::Search, s("pattern")),
        "LS" => (ToolKind::Search, s("path")),
        "WebFetch" => (ToolKind::Fetch, s("url")),
        "WebSearch" => (ToolKind::Fetch, s("query")),
        "Task" | "Agent" => (ToolKind::Think, s("description")),
        _ => (ToolKind::Other, String::new()),
    };
    let title = if what.is_empty() { name.to_string() } else { format!("{name} {}", cut(&what, 300).0) };
    (kind, title)
}

/// A `tool_use` block as a tool call (or, for the to-do list, a plan).
fn tool_call(st: &mut State, block: &Value, parent: Option<&str>) -> Option<Out> {
    let call_id = block["id"].as_str()?.to_string();
    let name = block["name"].as_str().unwrap_or("tool");
    let input = &block["input"];
    if name == "TodoWrite" && parent.is_none() {
        let entries = input["todos"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| PlanEntry {
                content: t["content"].as_str().unwrap_or("").to_string(),
                status: t["status"].as_str().unwrap_or("pending").to_string(),
            })
            .collect();
        return Some(Out::Ev(Ev::Plan { entries }));
    }
    st.calls.insert(call_id.clone());
    let s = |k: &str| input[k].as_str().unwrap_or("").to_string();
    let (kind, title) = describe(name, input);
    let path = input["file_path"].as_str().map(str::to_string);
    let mut content = Vec::new();
    match name {
        "Edit" => content.push(ToolContent::Diff {
            path: s("file_path"),
            old: Some(cut(&s("old_string"), DIFF_SIDE).0),
            new: cut(&s("new_string"), DIFF_SIDE).0,
            hunks: Vec::new(),
        }),
        "Write" => content.push(ToolContent::Diff {
            path: s("file_path"),
            old: None,
            new: cut(&s("content"), DIFF_SIDE).0,
            hunks: Vec::new(),
        }),
        "MultiEdit" => {
            for e in input["edits"].as_array().into_iter().flatten().take(20) {
                content.push(ToolContent::Diff {
                    path: s("file_path"),
                    old: Some(cut(e["old_string"].as_str().unwrap_or(""), DIFF_SIDE).0),
                    new: cut(e["new_string"].as_str().unwrap_or(""), DIFF_SIDE).0,
                    hunks: Vec::new(),
                });
            }
        }
        _ => {}
    }
    let shown_input = if content.is_empty() { Some(preview_value(input)) } else { None };
    Some(Out::Ev(Ev::ToolCall {
        call_id,
        parent: parent.map(str::to_string),
        title,
        kind,
        status: ToolStatus::InProgress,
        input: shown_input,
        locations: path.into_iter().collect(),
        content,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_lines(st: &mut State, lines: &[Value]) -> (Vec<String>, Vec<Out>) {
        let (mut w, mut o) = (Vec::new(), Vec::new());
        for l in lines {
            let (a, b) = handle(st, &l.to_string());
            w.extend(a);
            o.extend(b);
        }
        (w, o)
    }

    #[test]
    fn a_turn_streams_text_tools_and_ends_with_its_result() {
        let mut st = State::new("bypassPermissions");
        let (writes, outs) = run_lines(
            &mut st,
            &[
                json!({"type": "system", "subtype": "init", "session_id": "s1"}),
                json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "m1"}}}),
                json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hel"}}}),
                json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "lo"}}}),
                json!({"type": "assistant", "message": {"id": "m1", "content": [
                    {"type": "text", "text": "Hello"},
                    {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls -la", "run_in_background": true}},
                ]}}),
                json!({"type": "control_request", "request_id": "r1", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls", "run_in_background": true}}}),
                json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "a\nb"}]}]}}),
                json!({"type": "stream_event", "parent_tool_use_id": "x", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "nested"}}}),
                json!({"type": "result", "subtype": "success", "is_error": false, "result": "Hello", "usage": {"input_tokens": 3, "output_tokens": 2}, "total_cost_usd": 0.01}),
            ],
        );
        assert_eq!(writes.len(), 1);
        let reply: Value = serde_json::from_str(&writes[0]).unwrap();
        assert_eq!(reply["response"]["response"]["behavior"], "allow");
        assert_eq!(reply["response"]["response"]["updatedInput"]["run_in_background"], false);
        let shown: Vec<String> = outs.iter().map(|o| format!("{o:?}")).collect();
        assert!(matches!(&outs[0], Out::Session(s) if s == "s1"));
        assert!(matches!(&outs[1], Out::Ev(Ev::Message { text, delta: true, .. }) if text == "Hel"));
        assert!(matches!(&outs[2], Out::Ev(Ev::Message { text, delta: true, .. }) if text == "lo"));
        assert!(
            matches!(&outs[3], Out::Ev(Ev::ToolCall { kind: ToolKind::Execute, title, .. }) if title == "Bash ls -la"),
            "{shown:?}"
        );
        assert!(matches!(&outs[4], Out::Ev(Ev::ToolUpdate { status: Some(ToolStatus::Completed), .. })));
        assert!(matches!(&outs[5], Out::Ev(Ev::Usage { output_tokens: Some(2), .. })));
        assert!(
            matches!(&outs[6], Out::TurnEnd { status: TurnStatus::Completed, final_text: Some(t), .. } if t == "Hello")
        );
        assert_eq!(outs.len(), 7, "{shown:?}");
    }

    #[test]
    fn errors_and_interrupts_end_the_turn_accordingly() {
        let mut st = State::new("bypassPermissions");
        let (_, outs) = run_lines(
            &mut st,
            &[
                json!({"type": "result", "subtype": "success", "is_error": true, "result": "API Error: 401 token expired"}),
            ],
        );
        assert!(
            matches!(&outs[1], Out::TurnEnd { status: TurnStatus::Failed, reason: Some(r), .. } if r.contains("401"))
        );
        st.interrupting = true;
        let (_, outs) =
            run_lines(&mut st, &[json!({"type": "result", "subtype": "error_during_execution", "is_error": true})]);
        assert!(matches!(&outs[1], Out::TurnEnd { status: TurnStatus::Interrupted, reason: None, .. }));
        assert!(!st.interrupting);
    }

    #[test]
    fn edits_carry_their_diff_and_todos_become_a_plan() {
        let mut st = State::new("bypassPermissions");
        let (_, outs) = run_lines(
            &mut st,
            &[json!({"type": "assistant", "message": {"id": "m", "content": [
                {"type": "tool_use", "id": "e", "name": "Edit", "input": {"file_path": "/p/a.rs", "old_string": "x", "new_string": "y"}},
                {"type": "tool_use", "id": "p", "name": "TodoWrite", "input": {"todos": [{"content": "build", "status": "in_progress"}]}},
            ]}})],
        );
        assert!(
            matches!(&outs[0], Out::Ev(Ev::ToolCall { kind: ToolKind::Edit, content, input: None, .. }) if matches!(&content[0], ToolContent::Diff { new, .. } if new == "y"))
        );
        assert!(matches!(&outs[1], Out::Ev(Ev::Plan { entries }) if entries[0].status == "in_progress"));
        assert!(!st.calls.contains("p"));
    }

    #[test]
    fn arguments_put_bypass_and_resume_in_place() {
        let r = crate::chat::detect::Resolved {
            id: "claude".into(),
            name: "Claude Code".into(),
            runtime: crate::chat::runtimes::builtin("claude").unwrap(),
            caps: crate::chat::runtimes::builtin("claude").unwrap().caps,
            program: "claude".into(),
            prefix: vec![],
            env: Default::default(),
            resolved_by: "path",
        };
        let l = super::super::Launch {
            resolved: &r,
            cwd: std::path::Path::new("/"),
            model: Some("haiku"),
            hardened: true,
            resume: Some("s1"),
            permission_mode: None,
        };
        let a = args(&l).join(" ");
        assert!(a.contains("--permission-mode bypassPermissions"), "{a}");
        assert!(a.contains("--permission-prompt-tool stdio"), "{a}");
        assert!(!a.contains("--disallowedTools"), "{a}");
        let l = super::super::Launch { permission_mode: Some("plan"), ..l };
        assert!(args(&l).join(" ").contains("--permission-mode plan"));
        // A mode Claude Code may not know falls back to the default.
        let l = super::super::Launch { permission_mode: Some("yolo"), ..l };
        assert!(args(&l).join(" ").contains("--permission-mode bypassPermissions"));
        assert!(a.contains("--setting-sources user --strict-mcp-config"), "{a}");
        assert!(a.ends_with("--model haiku --resume s1"), "{a}");
    }

    fn replay() -> Value {
        json!({"type": "user", "isReplay": true, "message": {"role": "user", "content": [{"type": "text", "text": "x"}]}})
    }

    #[test]
    fn steers_echoed_before_the_result_joined_the_turn_and_the_rest_run_next() {
        let mut st = State::new("bypassPermissions");
        st.written.push_back(Written { steer: None, late: false });
        st.written.push_back(Written { steer: Some("s1".into()), late: false });
        st.written.push_back(Written { steer: Some("s2".into()), late: false });
        let (_, outs) = run_lines(
            &mut st,
            &[
                replay(),
                json!({"type": "assistant", "message": {"id": "a", "model": "claude-x", "usage": {"input_tokens": 5, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 3, "output_tokens": 2}, "content": [{"type": "text", "text": "hi"}]}}),
                replay(),
                json!({"type": "result", "subtype": "success", "is_error": false, "result": "done", "modelUsage": {"claude-x[1m]": {"contextWindow": 1000000}, "claude-haiku": {"contextWindow": 200000}}}),
            ],
        );
        let shown: Vec<String> = outs.iter().map(|o| format!("{o:?}")).collect();
        assert!(matches!(&outs[1], Out::Steered { id } if id == "s1"), "{shown:?}");
        assert!(
            matches!(&outs[2], Out::Ev(Ev::Usage { context_used: Some(110), context_window: Some(1000000), .. })),
            "{shown:?}"
        );
        assert!(matches!(&outs[3], Out::SteerLate { id } if id == "s2"), "{shown:?}");
        assert!(matches!(&outs[4], Out::TurnEnd { .. }), "{shown:?}");
        // s2 opens the next turn: its echo is that turn's message, not a steer of it.
        let (_, outs) =
            run_lines(&mut st, &[json!({"type": "system", "subtype": "init", "session_id": "s"}), replay()]);
        assert!(!outs.iter().any(|o| matches!(o, Out::Steered { .. })));
        assert!(st.written.is_empty());
    }

    #[test]
    fn commands_come_from_initialize_and_skills_from_init() {
        let mut st = State::new("bypassPermissions");
        let (_, outs) = run_lines(
            &mut st,
            &[
                json!({"type": "control_response", "response": {"subtype": "success", "request_id": "ar-init", "response": {"commands": [
                    {"name": "compact", "description": "Clear history but keep a summary", "argumentHint": "<instructions>"},
                    {"name": "gpu-debug", "description": "Attribute a rendering problem (user)", "argumentHint": ""},
                ]}}}),
            ],
        );
        // Held until an `init` says which are skills.
        assert!(outs.is_empty());
        let (_, outs) = run_lines(
            &mut st,
            &[json!({"type": "system", "subtype": "init", "session_id": "s", "skills": ["gpu-debug"]})],
        );
        let Some(Out::Ev(Ev::Commands { commands })) = outs.last() else { panic!("{outs:?}") };
        assert_eq!(
            (commands[0].kind.as_str(), commands[0].insert.as_str(), commands[0].hint.as_deref()),
            ("command", "/compact", Some("<instructions>"))
        );
        assert_eq!((commands[1].kind.as_str(), commands[1].hint.as_deref()), ("skill", None));
        // An agent that does not answer `initialize` still names its commands in `init`.
        let mut old = State::new("bypassPermissions");
        let (_, outs) = run_lines(
            &mut old,
            &[json!({"type": "system", "subtype": "init", "session_id": "s", "slash_commands": ["clear", "review"]})],
        );
        assert!(
            matches!(outs.last(), Some(Out::Ev(Ev::Commands { commands })) if commands.len() == 2 && commands[1].insert == "/review")
        );
    }

    #[test]
    fn images_go_first_as_base64_blocks() {
        let img = crate::chat::Image::parse("a.png", "image/png", "iVBORw0KGgo=").unwrap();
        let line: Value = serde_json::from_str(&user_line("what is this", &[img])).unwrap();
        let content = &line["message"]["content"];
        assert_eq!(content[0]["source"], json!({"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}));
        assert_eq!(content[1]["text"], "what is this");
        // An image alone: no empty text block.
        let img = crate::chat::Image::parse("a.png", "image/png", "iVBORw0KGgo=").unwrap();
        let line: Value = serde_json::from_str(&user_line("", &[img])).unwrap();
        assert_eq!(line["message"]["content"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_sub_agents_tool_calls_come_with_their_parent() {
        let mut st = State::new("bypassPermissions");
        let (_, outs) = run_lines(
            &mut st,
            &[
                json!({"type": "assistant", "message": {"id": "m1", "content": [{"type": "tool_use", "id": "a1", "name": "Agent", "input": {"description": "Find the date code", "prompt": "..."}}]}}),
                json!({"type": "assistant", "parent_tool_use_id": "a1", "message": {"id": "s1", "content": [
                    {"type": "text", "text": "Looking."},
                    {"type": "tool_use", "id": "g1", "name": "Grep", "input": {"pattern": "monthrange"}}]}}),
                json!({"type": "user", "parent_tool_use_id": "a1", "message": {"content": [{"type": "tool_result", "tool_use_id": "g1", "content": "export.py:2"}]}}),
                json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "a1", "content": [{"type": "text", "text": "It is in export.py"}]}]}}),
            ],
        );
        assert!(
            matches!(&outs[0], Out::Ev(Ev::ToolCall { call_id, parent: None, kind: ToolKind::Think, .. }) if call_id == "a1")
        );
        assert!(
            matches!(&outs[1], Out::Ev(Ev::ToolCall { call_id, parent: Some(p), kind: ToolKind::Search, .. }) if call_id == "g1" && p == "a1")
        );
        assert!(
            matches!(&outs[2], Out::Ev(Ev::ToolUpdate { call_id, status: Some(ToolStatus::Completed), .. }) if call_id == "g1")
        );
        assert!(matches!(&outs[3], Out::Ev(Ev::ToolUpdate { call_id, .. }) if call_id == "a1"));
        // The sub-agent's own text is not shown as the agent's.
        assert_eq!(outs.len(), 4, "{outs:?}");
    }

    fn can_use(id: &str, tool: &str, input: Value, extra: Value) -> Value {
        let mut req =
            json!({"subtype": "can_use_tool", "tool_name": tool, "input": input, "tool_use_id": format!("tu-{id}")});
        for (k, v) in extra.as_object().into_iter().flatten() {
            req[k] = v.clone();
        }
        json!({"type": "control_request", "request_id": id, "request": req})
    }

    fn written(w: &str) -> Value {
        serde_json::from_str(w).unwrap()
    }

    #[test]
    fn questions_come_in_bypass_mode_with_every_part_and_answers_go_back_per_part() {
        // As measured (2.1.308): AskUserQuestion is asked even in bypass mode.
        let mut st = State::new("bypassPermissions");
        let input = json!({"questions": [
            {"question": "Which config?", "header": "Config", "multiSelect": false,
             "options": [{"label": "debug", "description": "fast"}, {"label": "release"}]},
            {"question": "Which targets?", "header": "Targets", "multiSelect": true, "options": [{"label": "mac"}, {"label": "win"}]},
        ]});
        let (w, outs) = run_lines(&mut st, &[can_use("r1", "AskUserQuestion", input.clone(), json!({}))]);
        assert!(w.is_empty(), "nothing is answered until a person does");
        let Some(Out::Ev(Ev::Question { qid, prompt, options, questions, .. })) = outs.first() else {
            panic!("{outs:?}")
        };
        assert_eq!((prompt.as_str(), options.len(), questions.len()), ("Which config?", 2, 2));
        assert_eq!(questions[0].options[0].description.as_deref(), Some("fast"));
        assert!(questions[1].multi && !questions[0].multi);
        let qid = qid.clone();
        let answers: BTreeMap<String, String> =
            [("Which config?".into(), "release".into()), ("Which targets?".into(), "mac, win".into())].into();
        let (w, outs) =
            command(&mut st, LiveCmd::Answer { qid: qid.clone(), text: "release".into(), answers: Some(answers) });
        assert!(outs.is_empty());
        let r = written(&w[0]);
        assert_eq!(r["response"]["request_id"], "r1");
        assert_eq!(r["response"]["response"]["behavior"], "allow");
        assert_eq!(r["response"]["response"]["updatedInput"]["questions"], input["questions"]);
        assert_eq!(r["response"]["response"]["updatedInput"]["answers"]["Which targets?"], "mac, win");
        // Answered once: a second answer finds nothing.
        let (w, outs) = command(&mut st, LiveCmd::Answer { qid, text: "x".into(), answers: None });
        assert!(w.is_empty() && matches!(&outs[0], Out::Ev(Ev::Notice { code: "no_question", .. })));
        // An older controller's answer (text only) answers the first part.
        let (_, outs) = run_lines(&mut st, &[can_use("r2", "AskUserQuestion", input, json!({}))]);
        let Some(Out::Ev(Ev::Question { qid, .. })) = outs.first() else { panic!() };
        let (w, _) = command(&mut st, LiveCmd::Answer { qid: qid.clone(), text: "debug".into(), answers: None });
        assert_eq!(
            written(&w[0])["response"]["response"]["updatedInput"]["answers"],
            json!({"Which config?": "debug"})
        );
    }

    #[test]
    fn a_plan_is_reviewed_then_approved_in_a_mode_or_sent_back() {
        let mut st = State::new("plan");
        let input = json!({"plan": "## Plan\n1. build", "planFilePath": "/p/plan.md"});
        let (w, outs) = run_lines(&mut st, &[can_use("r1", "ExitPlanMode", input.clone(), json!({}))]);
        assert!(w.is_empty());
        let Some(Out::Ev(Ev::PlanReview { rid, plan })) = outs.first() else { panic!("{outs:?}") };
        assert_eq!(plan, "## Plan\n1. build");
        let (w, _) = command(
            &mut st,
            LiveCmd::Permit {
                rid: rid.clone(),
                decision: Decision::Once,
                message: None,
                mode: Some("acceptEdits".into()),
            },
        );
        assert_eq!(written(&w[0])["response"]["response"], json!({"behavior": "allow", "updatedInput": input}));
        let m = written(&w[1]);
        assert_eq!(m["request"], json!({"subtype": "set_permission_mode", "mode": "acceptEdits"}));
        // The switch is reported as the agent confirms it.
        let id = m["request_id"].as_str().unwrap().to_string();
        let (_, outs) = run_lines(
            &mut st,
            &[
                json!({"type": "control_response", "response": {"subtype": "success", "request_id": id, "response": {"mode": "acceptEdits"}}}),
            ],
        );
        assert!(matches!(&outs[0], Out::Ev(Ev::Mode { mode }) if mode == "acceptEdits"));
        // Sent back with what to change: denied, and the model reads why.
        let (_, outs) = run_lines(&mut st, &[can_use("r2", "ExitPlanMode", input, json!({}))]);
        let Some(Out::Ev(Ev::PlanReview { rid, .. })) = outs.first() else { panic!() };
        let (w, _) = command(
            &mut st,
            LiveCmd::Permit { rid: rid.clone(), decision: Decision::Deny, message: Some("not CI".into()), mode: None },
        );
        assert_eq!(
            written(&w[0])["response"]["response"],
            json!({"behavior": "deny", "message": "not CI", "interrupt": false})
        );
        assert_eq!(w.len(), 1, "no mode switch");
    }

    #[test]
    fn tools_ask_outside_bypass_mode_once_for_the_session_or_not() {
        let mut st = State::new("default");
        let suggestion = json!({"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "cargo build"}], "behavior": "allow", "destination": "localSettings"});
        let extra =
            json!({"permission_suggestions": [suggestion], "decision_reason": "This command requires approval"});
        let ask = |st: &mut State, id: &str| {
            let (w, outs) = run_lines(
                st,
                &[can_use(id, "Bash", json!({"command": "cargo build", "run_in_background": true}), extra.clone())],
            );
            assert!(w.is_empty());
            let Some(Out::Ev(Ev::Permission { rid, tool, kind, title, reason, suggestions, call_id, .. })) =
                outs.first()
            else {
                panic!("{outs:?}")
            };
            assert_eq!(
                (tool.as_str(), *kind, title.as_deref()),
                ("Bash", Some(ToolKind::Execute), Some("Bash cargo build"))
            );
            assert_eq!(
                (reason.as_deref(), suggestions.as_slice()),
                (Some("This command requires approval"), &["session".to_string()][..])
            );
            assert_eq!(call_id.as_deref(), Some(format!("tu-{id}").as_str()));
            rid.clone()
        };
        let rid = ask(&mut st, "r1");
        let (w, _) = command(&mut st, LiveCmd::Permit { rid, decision: Decision::Once, message: None, mode: None });
        let r = written(&w[0])["response"]["response"].clone();
        assert_eq!((r["behavior"].as_str(), &r["updatedInput"]["run_in_background"]), (Some("allow"), &json!(false)));
        assert!(r.get("updatedPermissions").is_none());
        // For the session: the agent's own rule, kept for this session (not written to the project).
        let rid = ask(&mut st, "r2");
        let (w, _) = command(&mut st, LiveCmd::Permit { rid, decision: Decision::Session, message: None, mode: None });
        let r = written(&w[0])["response"]["response"].clone();
        assert_eq!(r["updatedPermissions"][0]["destination"], "session");
        assert_eq!(r["updatedPermissions"][0]["rules"][0]["ruleContent"], "cargo build");
        // No without a word stops the turn; with one the model goes on.
        let rid = ask(&mut st, "r3");
        let (w, _) = command(&mut st, LiveCmd::Permit { rid, decision: Decision::Deny, message: None, mode: None });
        assert_eq!(written(&w[0])["response"]["response"]["interrupt"], true);
        // An answer to what is no longer asked.
        let (w, outs) =
            command(&mut st, LiveCmd::Permit { rid: "p0".into(), decision: Decision::Once, message: None, mode: None });
        assert!(w.is_empty() && matches!(&outs[0], Out::Ev(Ev::Notice { code: "permission_expired", .. })));
        // In bypass mode nothing is asked about tools.
        let mut st = State::new("bypassPermissions");
        let (w, outs) = run_lines(&mut st, &[can_use("r4", "Bash", json!({"command": "ls"}), extra)]);
        assert!(outs.is_empty() && written(&w[0])["response"]["response"]["behavior"] == "allow");
    }

    #[test]
    fn what_is_still_asked_falls_away_with_an_interrupt_or_the_turn() {
        let mut st = State::new("default");
        let (_, outs) =
            run_lines(&mut st, &[can_use("r1", "Write", json!({"file_path": "/a", "content": "x"}), json!({}))]);
        let Some(Out::Ev(Ev::Permission { rid, suggestions, .. })) = outs.first() else { panic!() };
        assert!(suggestions.is_empty());
        let rid = rid.clone();
        let (w, outs) = command(&mut st, LiveCmd::Interrupt);
        assert_eq!(written(&w[0])["response"]["response"]["behavior"], "deny");
        assert_eq!(written(&w[1])["request"]["subtype"], "interrupt");
        assert!(matches!(&outs[0], Out::Ev(Ev::Resolved { id, outcome: Outcome::Cancelled, .. }) if *id == rid));
        let (_, outs) = run_lines(
            &mut st,
            &[
                can_use("r2", "Edit", json!({"file_path": "/a"}), json!({})),
                json!({"type": "result", "subtype": "success", "is_error": false, "result": "x"}),
            ],
        );
        assert!(matches!(&outs[2], Out::Ev(Ev::Resolved { outcome: Outcome::Cancelled, .. })), "{outs:?}");
        // The agent took a request back.
        let (_, outs) = run_lines(
            &mut st,
            &[
                can_use("r3", "Edit", json!({"file_path": "/a"}), json!({})),
                json!({"type": "control_cancel_request", "request_id": "r3"}),
            ],
        );
        assert!(matches!(&outs[1], Out::Ev(Ev::Resolved { outcome: Outcome::Cancelled, .. })), "{outs:?}");
        assert!(st.asks.is_empty());
    }

    #[test]
    fn modes_come_from_system_lines_and_titles_from_their_answer() {
        let mut st = State::new("bypassPermissions");
        let (_, outs) = run_lines(
            &mut st,
            &[
                json!({"type": "system", "subtype": "init", "session_id": "s", "permissionMode": "bypassPermissions"}),
                json!({"type": "system", "subtype": "init", "session_id": "s", "permissionMode": "bypassPermissions"}),
                json!({"type": "system", "subtype": "status", "status": null, "permissionMode": "acceptEdits"}),
            ],
        );
        let modes: Vec<&str> = outs
            .iter()
            .filter_map(|o| if let Out::Ev(Ev::Mode { mode }) = o { Some(mode.as_str()) } else { None })
            .collect();
        assert_eq!(modes, ["bypassPermissions", "acceptEdits"], "said at start, then on each change");
        let (w, _) = command(&mut st, LiveCmd::SetMode { mode: "plan".into() });
        let id = written(&w[0])["request_id"].as_str().unwrap().to_string();
        let (_, outs) = run_lines(
            &mut st,
            &[
                json!({"type": "control_response", "response": {"subtype": "error", "request_id": id, "error": "Cannot set permission mode"}}),
            ],
        );
        assert!(matches!(&outs[0], Out::Ev(Ev::Notice { code: "mode_rejected", .. })));
        let (w, outs) = command(&mut st, LiveCmd::SetMode { mode: "yolo".into() });
        assert!(w.is_empty() && matches!(&outs[0], Out::Ev(Ev::Notice { code: "mode_rejected", .. })));
        // Title: the first message goes out, the answer's title comes back.
        let (w, _) = command(&mut st, LiveCmd::GenerateTitle { description: "Fix the build".into() });
        let req = written(&w[0]);
        assert_eq!(
            req["request"],
            json!({"subtype": "generate_session_title", "description": "Fix the build", "persist": false})
        );
        let id = req["request_id"].as_str().unwrap().to_string();
        let (_, outs) = run_lines(
            &mut st,
            &[
                json!({"type": "control_response", "response": {"subtype": "success", "request_id": id, "response": {"title": "Build fix"}}}),
            ],
        );
        assert!(matches!(&outs[0], Out::Ev(Ev::Title { title, source }) if title == "Build fix" && source == "agent"));
    }

    #[test]
    fn an_edit_result_carries_its_structured_patch_as_hunks() {
        let mut st = State::new("bypassPermissions");
        let (_, outs) = run_lines(
            &mut st,
            &[
                json!({"type": "assistant", "message": {"id": "m", "content": [{"type": "tool_use", "id": "e1", "name": "Edit", "input": {"file_path": "/w/t.py", "old_string": "b = 2", "new_string": "b = 20"}}]}}),
                // As measured (2.1.308): the result's `tool_use_result` has the patch.
                json!({"type": "user", "message": {"content": [{"tool_use_id": "e1", "type": "tool_result", "content": "updated"}]},
                       "tool_use_result": {"filePath": "/w/t.py", "oldString": "b = 2", "newString": "b = 20", "originalFile": "a = 1\nb = 2\nc = 3\n",
                         "structuredPatch": [{"oldStart": 1, "oldLines": 3, "newStart": 1, "newLines": 3, "lines": [" a = 1", "-b = 2", "+b = 20", " c = 3"]}]}}),
            ],
        );
        let Out::Ev(Ev::ToolUpdate { content, .. }) = &outs[1] else { panic!("{outs:?}") };
        let ToolContent::Diff { path, old, new, hunks } = &content[0] else { panic!("{content:?}") };
        assert_eq!((path.as_str(), old.as_deref(), new.as_str()), ("/w/t.py", Some("b = 2"), "b = 20"));
        assert_eq!((hunks[0].old_start, hunks[0].new_lines, hunks[0].lines.len()), (1, 3, 4));
        assert!(matches!(&content[1], ToolContent::Text { text } if text == "updated"));
        // A new file's empty patch adds nothing.
        let (_, outs) = run_lines(
            &mut st,
            &[
                json!({"type": "assistant", "message": {"id": "m2", "content": [{"type": "tool_use", "id": "w1", "name": "Write", "input": {"file_path": "/w/n", "content": "hi"}}]}}),
                json!({"type": "user", "message": {"content": [{"tool_use_id": "w1", "type": "tool_result", "content": "created"}]},
                       "tool_use_result": {"type": "create", "filePath": "/w/n", "content": "hi", "structuredPatch": []}}),
            ],
        );
        assert!(matches!(&outs[1], Out::Ev(Ev::ToolUpdate { content, .. }) if content.len() == 1));
    }
}
