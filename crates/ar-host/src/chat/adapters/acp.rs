//! Agents that speak the Agent Client Protocol (Gemini CLI, OpenCode, Qwen Code, Copilot CLI and
//! others; see [`crate::chat::runtimes`]): `initialize`, `session/new` (or, to continue,
//! `session/resume` or `session/load`, whichever the agent offers), then one `session/prompt` per
//! message, whose answer ends the turn. The agent reports progress with `session/update`
//! notifications, which already use this host's event vocabulary.
//!
//! - Bypass: the session switches to the agent's auto mode when it offers one ([`Runtime::auto_mode`]),
//!   and every `session/request_permission` is answered by the client: an "allow once" option if
//!   there is one, otherwise "reject once". Never "always allow": that would change the agent's
//!   stored settings beyond this session.
//! - The client offers no file system or terminal of its own (the agent uses its own tools), so
//!   `fs/*` and `terminal/*` requests are refused.
//! - Cancelling sends `session/cancel` and answers waiting permission requests with `cancelled`,
//!   as the protocol requires; the prompt then answers with stop reason `cancelled`.
//! - While `session/load` replays an earlier conversation, its updates are not shown again.
//!   `session/resume` (DeepSeek Harness) continues without replaying and is preferred.
//! - A model is chosen with the session's `model` config option when the agent lists one
//!   (`session/set_config_option`, matched by value or name), otherwise with `session/set_model`.
//! - Images go in the prompt as `image` blocks when the agent says it takes them
//!   (`promptCapabilities.image`); otherwise they are left out with a notice, and the session
//!   tells viewers (`agent` event) so the composer stops offering them.
//! - The agent's commands (`available_commands_update`) go to the `/` menu; choosing one sends
//!   `/name …` as the prompt, which is how ACP runs them.
//! - Text after a tool call is a new message (or each `messageId` its own), so a turn reads as the
//!   agent wrote it; the turn's final text is its last message.
//!
//! [`Runtime::auto_mode`]: crate::chat::runtimes::Runtime::auto_mode

use std::collections::{HashMap, VecDeque};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{LiveCmd, content_text, exit_reason, report};
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
    pub resume: Option<String>,
    pub auto_mode: Option<&'static str>,
    pub model: Option<String>,
}

pub fn args(l: &super::Launch<'_>) -> Vec<String> {
    l.resolved.runtime.args.iter().map(|s| s.to_string()).collect()
}

#[derive(Debug)]
enum Pending {
    Initialize,
    /// `continuing`: `session/resume` or `session/load` of [`Opts::resume`].
    Session {
        continuing: bool,
    },
    Prompt,
    Other,
}

#[derive(Default)]
struct State {
    next: u64,
    pending: HashMap<u64, Pending>,
    session: Option<String>,
    loading: bool,
    ready: bool,
    waiting: VecDeque<(String, Vec<Image>)>,
    /// The agent takes images (`promptCapabilities.image`); assumed until it says.
    images: Option<bool>,
    turn: u32,
    /// Text of the current message, the turn's final text if no other follows.
    text: String,
    /// Id of the current message within the turn.
    message: Option<String>,
    /// Bumped by a tool call: text after it is a new message.
    segment: u32,
    cancelling: bool,
    permissions: Vec<Value>,
}

impl State {
    fn request(&mut self, method: &str, params: Value, what: Pending) -> String {
        self.next += 1;
        self.pending.insert(self.next, what);
        jsonrpc::request(self.next, method, params)
    }

    /// The `session/prompt` for a message, and a notice when its images had to be left out.
    fn prompt(&mut self, text: &str, images: &[Image]) -> (String, Option<Out>) {
        let mut prompt: Vec<Value> = Vec::new();
        let mut dropped = None;
        if !images.is_empty() {
            if self.images == Some(false) {
                dropped = Some(Out::Ev(Ev::Notice {
                    input_id: None,
                    level: "warn",
                    code: "no_images",
                    text: format!("this agent takes no images; {} left out, the text was sent", images.len()),
                }));
            } else {
                prompt.extend(images.iter().map(|i| json!({"type": "image", "mimeType": i.mime, "data": &*i.data})));
            }
        }
        if !text.trim().is_empty() || prompt.is_empty() {
            prompt.push(json!({"type": "text", "text": text}));
        }
        (self.prompt_blocks(prompt), dropped)
    }

    fn prompt_blocks(&mut self, prompt: Vec<Value>) -> String {
        self.turn += 1;
        self.text.clear();
        self.message = None;
        self.segment = 0;
        self.cancelling = false;
        let session = self.session.clone().unwrap_or_default();
        self.request("session/prompt", json!({"sessionId": session, "prompt": prompt}), Pending::Prompt)
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
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
            "clientInfo": {"name": "ai-remote", "version": ar_core::VERSION},
        }),
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
                    let (writes, outs) = handle(&mut st, &line, &opts, &name);
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
                        let (line, dropped) = st.prompt(&text, &images);
                        if let Some(o) = dropped {
                            report(&out, o).await;
                        }
                        let _ = proc.write_line(line).await;
                    } else {
                        st.waiting.push_back((text, images));
                    }
                }
                Some(LiveCmd::Interrupt) => {
                    st.cancelling = true;
                    for id in std::mem::take(&mut st.permissions) {
                        let _ = proc.write_line(jsonrpc::response(&id, json!({"outcome": {"outcome": "cancelled"}}))).await;
                    }
                    match st.session.clone() {
                        Some(s) if st.ready => {
                            let _ = proc.write_line(jsonrpc::notification("session/cancel", Some(json!({"sessionId": s})))).await;
                        }
                        _ => {
                            st.waiting.clear();
                            report(&out, Out::TurnEnd { status: TurnStatus::Interrupted, final_text: None, reason: None }).await;
                        }
                    }
                }
                Some(LiveCmd::Answer { .. }) => {
                    report(&out, Out::Ev(Ev::Notice { input_id: None, level: "warn", code: "no_question", text: format!("{name} is not waiting for an answer") })).await;
                }
                Some(LiveCmd::Steer { id, .. }) => {
                    // ACP has no way to add to a running prompt turn.
                    report(&out, Out::SteerRejected { id, reason: "the protocol has no way to add to a running turn".into() }).await;
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

fn session_new(st: &mut State, opts: &Opts) -> String {
    st.request("session/new", json!({"cwd": opts.cwd, "mcpServers": []}), Pending::Session { continuing: false })
}

/// The request that selects `model`: the `model` config option's matching value when the agent
/// lists config options, `session/set_model` when it lists none. `Err` names what was offered.
fn select_model(st: &mut State, session: &str, r: &Value, model: &str) -> Result<String, String> {
    let Some(options) = r["configOptions"].as_array() else {
        return Ok(st.request("session/set_model", json!({"sessionId": session, "modelId": model}), Pending::Other));
    };
    let Some(opt) = options.iter().find(|o| o["category"] == "model" || o["id"] == "model") else {
        return Err("the agent offers no model choice".into());
    };
    // Options are flat or in groups; a value may be opaque (DeepSeek Harness: `["provider","model"]`).
    let choices: Vec<&Value> = opt["options"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|o| match o["options"].as_array() {
            Some(group) => group.iter().collect::<Vec<_>>(),
            None => vec![o],
        })
        .collect();
    let matches = |c: &&&Value| {
        let value = c["value"].as_str().unwrap_or("");
        value == model
            || c["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(model))
            || serde_json::from_str::<Vec<String>>(value).is_ok_and(|v| v.last().is_some_and(|m| m == model))
    };
    match choices.iter().find(matches).and_then(|c| c["value"].as_str()) {
        Some(value) if opt["currentValue"] == value => Err(String::new()),
        Some(value) => Ok(st.request(
            "session/set_config_option",
            json!({"sessionId": session, "configId": opt["id"], "value": value}),
            Pending::Other,
        )),
        None => {
            let names: Vec<&str> =
                choices.iter().filter_map(|c| c["name"].as_str().or(c["value"].as_str())).take(12).collect();
            Err(format!("offered: {}", names.join(", ")))
        }
    }
}

fn handle(st: &mut State, line: &str, opts: &Opts, name: &str) -> (Vec<String>, Vec<Out>) {
    let (mut writes, mut outs) = (Vec::new(), Vec::new());
    let Some(msg) = jsonrpc::parse(line) else { return (writes, outs) };
    match msg {
        Incoming::Response { id, result } => match (st.pending.remove(&id), result) {
            (Some(Pending::Initialize), Ok(r)) => {
                let caps = &r["agentCapabilities"];
                let images = caps["promptCapabilities"]["image"].as_bool() == Some(true);
                st.images = Some(images);
                outs.push(Out::Ev(Ev::Agent { images }));
                let can_resume = caps["sessionCapabilities"]["resume"].is_object();
                let can_load = caps["loadSession"].as_bool() == Some(true);
                match opts.resume.clone() {
                    Some(s) if can_resume || can_load => {
                        // `session/load` replays the conversation (hidden while `loading`);
                        // `session/resume` does not.
                        st.loading = !can_resume;
                        st.session = Some(s.clone());
                        let method = if can_resume { "session/resume" } else { "session/load" };
                        writes.push(st.request(
                            method,
                            json!({"sessionId": s, "cwd": opts.cwd, "mcpServers": []}),
                            Pending::Session { continuing: true },
                        ));
                    }
                    Some(_) => {
                        outs.push(Out::Ev(Ev::Notice {
                            input_id: None,
                            level: "warn",
                            code: "context_lost",
                            text: format!("{name} cannot continue an earlier conversation; a new one starts"),
                        }));
                        writes.push(session_new(st, opts));
                    }
                    None => writes.push(session_new(st, opts)),
                }
            }
            (Some(Pending::Initialize), Err(e)) => outs.push(Out::Exited(format!("{name} refused to start: {e}"))),
            (Some(Pending::Session { continuing }), Ok(r)) => {
                st.loading = false;
                if !continuing {
                    st.session = r["sessionId"].as_str().map(str::to_string);
                }
                let Some(session) = st.session.clone() else {
                    outs.push(Out::Exited(format!("{name} did not return a session")));
                    return (writes, outs);
                };
                if let Some(mode) = opts.auto_mode {
                    let offered =
                        r["modes"]["availableModes"].as_array().into_iter().flatten().any(|m| m["id"] == mode);
                    if offered && r["modes"]["currentModeId"] != mode {
                        writes.push(st.request(
                            "session/set_mode",
                            json!({"sessionId": session, "modeId": mode}),
                            Pending::Other,
                        ));
                    }
                }
                if let Some(model) = &opts.model {
                    match select_model(st, &session, &r, model) {
                        Ok(w) => writes.push(w),
                        Err(why) if why.is_empty() => {}
                        Err(why) => outs.push(Out::Ev(Ev::Notice {
                            input_id: None,
                            level: "warn",
                            code: "model",
                            text: format!("{name} has no model '{model}' ({why}); it uses its own"),
                        })),
                    }
                }
                st.ready = true;
                outs.push(Out::Session(session));
                while let Some((text, images)) = st.waiting.pop_front() {
                    let (line, dropped) = st.prompt(&text, &images);
                    outs.extend(dropped);
                    writes.push(line);
                }
            }
            (Some(Pending::Session { continuing: true }), Err(e)) => {
                st.loading = false;
                st.session = None;
                outs.push(Out::Ev(Ev::Notice {
                    input_id: None,
                    level: "warn",
                    code: "context_lost",
                    text: format!("{name} could not continue the earlier conversation ({e}); a new one starts"),
                }));
                writes.push(session_new(st, opts));
            }
            (Some(Pending::Session { continuing: false }), Err(e)) => {
                outs.push(Out::Exited(format!("{name} could not start a session: {e}")))
            }
            (Some(Pending::Prompt), r) => {
                let text = std::mem::take(&mut st.text);
                let (status, reason) = match &r {
                    Ok(v) => match v["stopReason"].as_str().unwrap_or("end_turn") {
                        "cancelled" => (TurnStatus::Interrupted, None),
                        "refusal" => (TurnStatus::Failed, Some("the agent refused".to_string())),
                        "max_tokens" | "max_turn_requests" | "end_turn" => (TurnStatus::Completed, None),
                        other => (TurnStatus::Completed, Some(other.to_string())),
                    },
                    Err(e) if st.cancelling => (TurnStatus::Interrupted, Some(e.clone())),
                    Err(e) => (TurnStatus::Failed, Some(e.clone())),
                };
                st.cancelling = false;
                let final_text = (status == TurnStatus::Completed && !text.is_empty()).then_some(text);
                outs.push(Out::TurnEnd { status, final_text, reason });
            }
            (Some(Pending::Other), Err(e)) => {
                outs.push(Out::Ev(Ev::Notice { input_id: None, level: "warn", code: "rpc_error", text: e }))
            }
            _ => {}
        },
        Incoming::Request { id, method, params } => match method.as_str() {
            "session/request_permission" => {
                if st.cancelling {
                    writes.push(jsonrpc::response(&id, json!({"outcome": {"outcome": "cancelled"}})));
                } else {
                    let options: Vec<&Value> = params["options"].as_array().into_iter().flatten().collect();
                    let pick = options
                        .iter()
                        .find(|o| o["kind"] == "allow_once")
                        .or_else(|| options.iter().find(|o| o["kind"] == "reject_once"))
                        .and_then(|o| o["optionId"].as_str());
                    match pick {
                        Some(option) => writes.push(jsonrpc::response(
                            &id,
                            json!({"outcome": {"outcome": "selected", "optionId": option}}),
                        )),
                        None => writes.push(jsonrpc::response(&id, json!({"outcome": {"outcome": "cancelled"}}))),
                    }
                }
            }
            other => writes.push(jsonrpc::unsupported(&id, other)),
        },
        Incoming::Notification { method, params } => {
            let commands = params["update"]["sessionUpdate"] == "available_commands_update";
            if method == "session/update"
                && (!st.loading || commands)
                && params["sessionId"].as_str() == st.session.as_deref()
            {
                update(st, &params["update"], &mut outs);
            }
        }
    }
    (writes, outs)
}

fn update(st: &mut State, u: &Value, outs: &mut Vec<Out>) {
    match u["sessionUpdate"].as_str().unwrap_or("") {
        "agent_message_chunk" => {
            let text = chunk_text(&u["content"]);
            let id = match u["messageId"].as_str() {
                Some(m) => format!("t{}:{m}", st.turn),
                None => format!("t{}.{}", st.turn, st.segment),
            };
            if st.message.as_ref() != Some(&id) {
                st.message = Some(id.clone());
                st.text.clear();
            }
            st.text.push_str(&text);
            outs.push(Out::Ev(Ev::Message { id, text, delta: true, phase: None }));
        }
        "agent_thought_chunk" => {
            outs.push(Out::Ev(Ev::Thought {
                id: match u["messageId"].as_str() {
                    Some(m) => format!("t{}:{m}r", st.turn),
                    None => format!("t{}.{}r", st.turn, st.segment),
                },
                text: chunk_text(&u["content"]),
                delta: true,
            }));
        }
        "tool_call" => {
            st.segment += 1;
            let (title, kind, locations) = describe_call(u);
            outs.push(Out::Ev(Ev::ToolCall {
                call_id: u["toolCallId"].as_str().unwrap_or("").to_string(),
                parent: None,
                title,
                kind,
                status: status(u["status"].as_str()).unwrap_or(ToolStatus::Pending),
                input: u.get("rawInput").filter(|v| !v.is_null()).map(preview_value),
                locations,
                content: contents(&u["content"]).0,
            }))
        }
        "tool_call_update" => {
            let (content, truncated) = contents(&u["content"]);
            outs.push(Out::Ev(Ev::ToolUpdate {
                call_id: u["toolCallId"].as_str().unwrap_or("").to_string(),
                status: status(u["status"].as_str()),
                content,
                truncated,
                exit_code: None,
                duration_ms: None,
            }));
        }
        "plan" => {
            let entries = u["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| PlanEntry {
                    content: e["content"].as_str().unwrap_or("").to_string(),
                    status: e["status"].as_str().unwrap_or("pending").to_string(),
                })
                .collect();
            outs.push(Out::Ev(Ev::Plan { entries }));
        }
        "available_commands_update" => {
            // Run as typed: the agent reads a prompt that starts with `/name`.
            let commands = u["availableCommands"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| {
                    let name = c["name"].as_str()?.trim_start_matches('/');
                    Some(SlashCommand::new(
                        "command",
                        name,
                        format!("/{name}"),
                        c["description"].as_str().unwrap_or(""),
                        c["input"]["hint"].as_str(),
                    ))
                })
                .collect();
            outs.push(Out::Ev(Ev::Commands { commands }));
        }
        "usage_update" => outs.push(Out::Ev(Ev::Usage {
            input_tokens: None,
            output_tokens: None,
            cost_usd: u["cost"]["amount"].as_f64().filter(|_| u["cost"]["currency"] == "USD"),
            context_window: u["size"].as_u64(),
            context_used: u["used"].as_u64(),
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
        })),
        _ => {}
    }
}

fn chunk_text(c: &Value) -> String {
    match c["type"].as_str() {
        Some("text") => c["text"].as_str().unwrap_or("").to_string(),
        Some(other) => format!("[{other}]"),
        None => String::new(),
    }
}

fn kind(k: Option<&str>) -> ToolKind {
    match k.unwrap_or("other") {
        "read" => ToolKind::Read,
        "edit" => ToolKind::Edit,
        "delete" => ToolKind::Delete,
        "move" => ToolKind::Move,
        "search" => ToolKind::Search,
        "execute" => ToolKind::Execute,
        "think" => ToolKind::Think,
        "fetch" => ToolKind::Fetch,
        _ => ToolKind::Other,
    }
}

fn status(s: Option<&str>) -> Option<ToolStatus> {
    Some(match s? {
        "pending" => ToolStatus::Pending,
        "in_progress" => ToolStatus::InProgress,
        "completed" => ToolStatus::Completed,
        "failed" => ToolStatus::Failed,
        _ => return None,
    })
}

fn locations(u: &Value) -> Vec<String> {
    u["locations"].as_array().into_iter().flatten().filter_map(|l| l["path"].as_str().map(str::to_string)).collect()
}

/// A tool call's title, kind and paths. Agents that report every call as kind `other` with the
/// tool's name as its title (DeepSeek Harness: `bash`, `read`) get a kind guessed from that name, the
/// command or path from `rawInput` as the title, and that path as a location.
fn describe_call(u: &Value) -> (String, ToolKind, Vec<String>) {
    let title = u["title"].as_str().unwrap_or("tool");
    let mut locs = locations(u);
    let reported = kind(u["kind"].as_str());
    if reported != ToolKind::Other {
        return (cut(title, 400).0, reported, locs);
    }
    let input = &u["rawInput"];
    let path = ["file_path", "path", "filePath", "notebook_path"].iter().find_map(|k| input[*k].as_str());
    if locs.is_empty()
        && let Some(p) = path
    {
        locs.push(p.to_string());
    }
    let name = title.to_ascii_lowercase();
    let guessed = match name.as_str() {
        "bash" | "shell" | "sh" | "exec" | "execute" | "run" | "terminal" | "powershell" => ToolKind::Execute,
        "read" | "view" | "cat" | "ls" | "list" => ToolKind::Read,
        "write" | "edit" | "multiedit" | "multi_edit" | "apply_patch" | "patch" | "create" => ToolKind::Edit,
        "grep" | "glob" | "search" | "find" => ToolKind::Search,
        "delete" | "rm" => ToolKind::Delete,
        "move" | "mv" | "rename" => ToolKind::Move,
        "fetch" | "webfetch" | "web_fetch" | "websearch" | "web_search" => ToolKind::Fetch,
        _ => ToolKind::Other,
    };
    let subject = match guessed {
        ToolKind::Execute => input["command"].as_str().map(str::to_string),
        _ => path.map(|p| format!("{title} {p}")).or_else(|| {
            ["pattern", "query", "url"].iter().find_map(|k| input[*k].as_str()).map(|q| format!("{title} {q}"))
        }),
    };
    (cut(&subject.unwrap_or_else(|| title.to_string()), 400).0, guessed, locs)
}

/// A tool call's content blocks: text and diffs (terminals are the agent's own and not shown).
fn contents(v: &Value) -> (Vec<ToolContent>, bool) {
    let mut truncated = false;
    let blocks = v
        .as_array()
        .into_iter()
        .flatten()
        .take(20)
        .filter_map(|b| match b["type"].as_str()? {
            "content" => {
                let (text, cut_) = cut(&content_text(&json!([b["content"]])), PREVIEW);
                truncated |= cut_;
                Some(ToolContent::Text { text })
            }
            "diff" => Some(ToolContent::Diff {
                path: b["path"].as_str().unwrap_or("").to_string(),
                old: b["oldText"].as_str().map(|s| cut(s, DIFF_SIDE).0),
                new: cut(b["newText"].as_str().unwrap_or(""), DIFF_SIDE).0,
            }),
            _ => None,
        })
        .collect();
    (blocks, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(resume: Option<&str>) -> Opts {
        Opts { cwd: "/w".into(), resume: resume.map(str::to_string), auto_mode: Some("yolo"), model: None }
    }

    #[test]
    fn a_loaded_session_hides_its_replay_and_switches_to_auto_mode() {
        let mut st = State::default();
        let _ = st.request("initialize", json!({}), Pending::Initialize);
        st.waiting.push_back(("hi".into(), vec![]));
        let (w, _) =
            handle(&mut st, r#"{"id":1,"result":{"agentCapabilities":{"loadSession":true}}}"#, &opts(Some("s9")), "A");
        assert!(w[0].contains("session/load") && w[0].contains("s9"));
        let (_, o) = handle(
            &mut st,
            r#"{"method":"session/update","params":{"sessionId":"s9","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"old"}}}}"#,
            &opts(Some("s9")),
            "A",
        );
        assert!(o.is_empty(), "replayed updates are not shown");
        let load = r#"{"id":2,"result":{"modes":{"currentModeId":"default","availableModes":[{"id":"default"},{"id":"yolo"}]}}}"#;
        let (w, o) = handle(&mut st, load, &opts(Some("s9")), "A");
        assert!(matches!(&o[0], Out::Session(s) if s == "s9"));
        assert!(w[0].contains("session/set_mode") && w[0].contains("yolo"));
        assert!(w[1].contains("session/prompt") && w[1].contains("\"hi\""));
    }

    #[test]
    fn updates_permissions_and_the_prompt_answer_make_a_turn() {
        let mut st = State { session: Some("s".into()), ready: true, ..Default::default() };
        let (p, _) = st.prompt("go", &[]);
        assert!(p.contains("session/prompt"));
        let lines = [
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Do"}}}}"#,
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"k","title":"Run ls","kind":"execute","status":"pending"}}}"#,
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"k","status":"completed","content":[{"type":"content","content":{"type":"text","text":"a"}},{"type":"diff","path":"/w/x","oldText":"1","newText":"2"}]}}}"#,
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"ne"}}}}"#,
        ];
        let mut outs = Vec::new();
        for l in lines {
            outs.extend(handle(&mut st, l, &opts(None), "A").1);
        }
        assert!(matches!(&outs[1], Out::Ev(Ev::ToolCall { kind: ToolKind::Execute, status: ToolStatus::Pending, .. })));
        // Text after a tool call is a new message.
        assert!(
            matches!((&outs[0], &outs[3]), (Out::Ev(Ev::Message { id: a, .. }), Out::Ev(Ev::Message { id: b, .. })) if a != b)
        );
        assert!(matches!(&outs[2], Out::Ev(Ev::ToolUpdate { content, .. }) if content.len() == 2));
        let perm = r#"{"id":"p1","method":"session/request_permission","params":{"sessionId":"s","options":[{"optionId":"aa","kind":"allow_always"},{"optionId":"ao","kind":"allow_once"},{"optionId":"r","kind":"reject_once"}]}}"#;
        let (w, _) = handle(&mut st, perm, &opts(None), "A");
        assert!(w[0].contains("\"ao\""), "allow once, never allow always: {}", w[0]);
        let perm = r#"{"id":"p2","method":"session/request_permission","params":{"sessionId":"s","options":[{"optionId":"aa","kind":"allow_always"},{"optionId":"r","kind":"reject_once"}]}}"#;
        let (w, _) = handle(&mut st, perm, &opts(None), "A");
        assert!(w[0].contains("\"r\""), "{}", w[0]);
        let answer = format!(r#"{{"id":{},"result":{{"stopReason":"end_turn"}}}}"#, st.next);
        let (_, o) = handle(&mut st, &answer, &opts(None), "A");
        assert!(
            matches!(&o[0], Out::TurnEnd { status: TurnStatus::Completed, final_text: Some(t), .. } if t == "ne"),
            "{o:?}"
        );
        let (w, _) = handle(&mut st, r#"{"id":"f","method":"fs/read_text_file","params":{}}"#, &opts(None), "A");
        assert!(w[0].contains("-32601"));
    }

    /// DeepSeek Harness (`dsh --profile acp`): `session/resume` instead of `session/load`, models as
    /// an opaque config option, whole messages with ids, context usage. Lines as its server sends them.
    #[test]
    fn deepseek_harness_resumes_selects_its_model_and_reports_context() {
        let o = Opts {
            cwd: "/w".into(),
            resume: Some("u1".into()),
            auto_mode: None,
            model: Some("deepseek-v4-flash".into()),
        };
        let mut st = State::default();
        let _ = st.request("initialize", json!({}), Pending::Initialize);
        let init = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"deepseek-harness-acp","version":"0.0.1"},"agentCapabilities":{"mcpCapabilities":{"http":true},"promptCapabilities":{"image":false,"audio":false,"embeddedContext":false},"sessionCapabilities":{"close":{},"list":{},"resume":{}}},"authMethods":[]}}"#;
        let (w, _) = handle(&mut st, init, &o, "DeepSeek Harness");
        assert!(w[0].contains("session/resume") && w[0].contains("\"u1\""), "{}", w[0]);
        assert!(!st.loading, "resume does not replay");
        let resumed = r#"{"jsonrpc":"2.0","id":2,"result":{"configOptions":[{"id":"model","name":"Model","category":"model","type":"select","currentValue":"[\"deepseek-official\",\"deepseek-v4-pro\"]","options":[{"group":"deepseek-official","name":"DeepSeek","options":[{"value":"[\"deepseek-official\",\"deepseek-v4-pro\"]","name":"deepseek-v4-pro"},{"value":"[\"deepseek-official\",\"deepseek-v4-flash\"]","name":"deepseek-v4-flash"}]}]},{"id":"reasoning_effort","name":"Reasoning effort","category":"thought_level","type":"select","currentValue":"high","options":[{"value":"high","name":"High"}]}]}}"#;
        let (w, out) = handle(&mut st, resumed, &o, "DeepSeek Harness");
        assert!(matches!(&out[0], Out::Session(s) if s == "u1"));
        assert!(w[0].contains("session/set_config_option") && w[0].contains(r#"\"deepseek-v4-flash\"]"#), "{}", w[0]);
        assert!(!w.iter().any(|l| l.contains("session/set_model")));

        let (p, _) = st.prompt("go", &[]);
        assert!(p.contains("\"u1\""));
        let lines = [
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"u1","update":{"sessionUpdate":"agent_thought_chunk","messageId":"m1","content":{"type":"text","text":"Look first."}}}}"#,
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"u1","update":{"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"Listing."}}}}"#,
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"u1","update":{"sessionUpdate":"usage_update","used":18234,"size":131072}}}"#,
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"u1","update":{"sessionUpdate":"tool_call","toolCallId":"call_00_ET_D6g3ZEp1Y2NP0aGjGLek2144","title":"bash","kind":"other","status":"in_progress","rawInput":{"command":"sleep 3 && echo finished","description":"Sleep 3 seconds then echo finished"}}}}"#,
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"u1","update":{"sessionUpdate":"tool_call_update","toolCallId":"call_00_ET_D6g3ZEp1Y2NP0aGjGLek2144","status":"failed","content":[{"type":"content","content":{"type":"text","text":"Error: tool call aborted"}}]}}}"#,
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"u1","update":{"sessionUpdate":"agent_message_chunk","messageId":"m2","content":{"type":"text","text":"It is empty."}}}}"#,
        ];
        let mut outs = Vec::new();
        for l in lines {
            outs.extend(handle(&mut st, l, &o, "DeepSeek Harness").1);
        }
        assert!(matches!(&outs[0], Out::Ev(Ev::Thought { id, .. }) if id == "t1:m1r"));
        assert!(matches!(&outs[1], Out::Ev(Ev::Message { id, .. }) if id == "t1:m1"));
        assert!(matches!(&outs[2], Out::Ev(Ev::Usage { context_used: Some(18234), context_window: Some(131072), .. })));
        assert!(matches!(
            &outs[3],
            Out::Ev(Ev::ToolCall { status: ToolStatus::InProgress, kind: ToolKind::Execute, title, input: Some(_), .. })
                if title == "sleep 3 && echo finished"
        ));
        let read = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"u1","update":{"sessionUpdate":"tool_call","toolCallId":"call_01","title":"read","kind":"other","status":"in_progress","rawInput":{"file_path":"/etc/hosts","limit":2}}}}"#;
        let (_, r) = handle(&mut st, read, &o, "DeepSeek Harness");
        assert!(matches!(
            &r[0],
            Out::Ev(Ev::ToolCall { kind: ToolKind::Read, title, locations, .. }) if title == "read /etc/hosts" && locations == &["/etc/hosts"]
        ));
        assert!(matches!(&outs[4], Out::Ev(Ev::ToolUpdate { status: Some(ToolStatus::Failed), .. })));
        assert!(matches!(&outs[5], Out::Ev(Ev::Message { id, .. }) if id == "t1:m2"));
        let done = format!(r#"{{"jsonrpc":"2.0","id":{},"result":{{"stopReason":"end_turn"}}}}"#, st.next);
        let (_, end) = handle(&mut st, &done, &o, "DeepSeek Harness");
        assert!(
            matches!(&end[0], Out::TurnEnd { status: TurnStatus::Completed, final_text: Some(t), .. } if t == "It is empty."),
            "{end:?}"
        );
    }

    #[test]
    fn a_failed_resume_starts_a_new_session_and_an_unknown_model_is_reported() {
        let o = Opts { cwd: "/w".into(), resume: Some("gone".into()), auto_mode: None, model: Some("nope".into()) };
        let mut st = State::default();
        let _ = st.request("initialize", json!({}), Pending::Initialize);
        let (w, _) = handle(
            &mut st,
            r#"{"id":1,"result":{"agentCapabilities":{"sessionCapabilities":{"resume":{}}}}}"#,
            &o,
            "D",
        );
        assert!(w[0].contains("session/resume"));
        let (w, out) =
            handle(&mut st, r#"{"id":2,"error":{"code":-32602,"message":"session is not resumable: gone"}}"#, &o, "D");
        assert!(matches!(&out[0], Out::Ev(Ev::Notice { code: "context_lost", .. })));
        assert!(w[0].contains("session/new"));
        let new = r#"{"id":3,"result":{"sessionId":"n1","configOptions":[{"id":"model","category":"model","currentValue":"[\"p\",\"a\"]","options":[{"value":"[\"p\",\"a\"]","name":"a"}]}]}}"#;
        let (w, out) = handle(&mut st, new, &o, "D");
        assert!(w.is_empty(), "{w:?}");
        assert!(matches!(&out[0], Out::Ev(Ev::Notice { code: "model", text, .. }) if text.contains("offered: a")));
        assert!(matches!(&out[1], Out::Session(s) if s == "n1"));
    }

    #[test]
    fn the_agents_commands_reach_the_menu_even_while_loading() {
        let mut st = State { session: Some("s".into()), loading: true, ..Default::default() };
        let (_, outs) = handle(
            &mut st,
            r#"{"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"init","description":"Create AGENTS.md"},{"name":"review","description":"Review","input":{"hint":"branch"}}]}}}"#,
            &opts(None),
            "A",
        );
        let Some(Out::Ev(Ev::Commands { commands })) = outs.first() else { panic!("{outs:?}") };
        assert_eq!(commands.len(), 2);
        assert_eq!((commands[1].insert.as_str(), commands[1].hint.as_deref()), ("/review", Some("branch")));
    }

    #[test]
    fn images_go_in_the_prompt_only_when_the_agent_takes_them() {
        let img = || crate::chat::Image::parse("a.png", "image/png", "iVBORw0KGgo=").unwrap();
        let mut st = State { session: Some("s".into()), ..Default::default() };
        let (_, outs) = handle(
            &mut st,
            r#"{"id":0,"result":{"agentCapabilities":{"promptCapabilities":{"image":true}}}}"#,
            &opts(None),
            "A",
        );
        assert!(outs.is_empty(), "no request 0 was pending");
        st.pending.insert(7, Pending::Initialize);
        let (_, outs) = handle(
            &mut st,
            r#"{"id":7,"result":{"agentCapabilities":{"promptCapabilities":{"image":true}}}}"#,
            &opts(None),
            "A",
        );
        assert!(matches!(outs[0], Out::Ev(Ev::Agent { images: true })));
        let (p, dropped) = st.prompt("see", &[img()]);
        let p: Value = serde_json::from_str(&p).unwrap();
        assert_eq!(p["params"]["prompt"][0], json!({"type": "image", "mimeType": "image/png", "data": "iVBORw0KGgo="}));
        assert!(dropped.is_none());
        st.images = Some(false);
        let (p, dropped) = st.prompt("see", &[img()]);
        assert!(!p.contains("image/png"));
        assert!(matches!(dropped, Some(Out::Ev(Ev::Notice { code: "no_images", .. }))));
    }
}
