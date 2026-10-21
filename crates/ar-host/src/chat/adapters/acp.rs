//! Agents that speak the Agent Client Protocol (Gemini CLI, OpenCode, Qwen Code, Copilot CLI and
//! others; see [`crate::chat::runtimes`]): `initialize`, `session/new` (or `session/load` to
//! continue), then one `session/prompt` per message, whose answer ends the turn. The agent reports
//! progress with `session/update` notifications, which already use this host's event vocabulary.
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
//!
//! [`Runtime::auto_mode`]: crate::chat::runtimes::Runtime::auto_mode

use std::collections::{HashMap, VecDeque};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{LiveCmd, content_text, exit_reason, report};
use crate::chat::{
    event::{
        DIFF_SIDE, Ev, Out, PREVIEW, PlanEntry, ToolContent, ToolKind, ToolStatus, TurnStatus, cut, preview_value,
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
    Session { loading: bool },
    Prompt,
    Other,
}

#[derive(Default)]
struct State {
    next: u64,
    pending: HashMap<u64, Pending>,
    session: Option<String>,
    can_load: bool,
    loading: bool,
    ready: bool,
    waiting: VecDeque<String>,
    turn: u32,
    text: String,
    cancelling: bool,
    permissions: Vec<Value>,
}

impl State {
    fn request(&mut self, method: &str, params: Value, what: Pending) -> String {
        self.next += 1;
        self.pending.insert(self.next, what);
        jsonrpc::request(self.next, method, params)
    }

    fn prompt(&mut self, text: &str) -> String {
        self.turn += 1;
        self.text.clear();
        self.cancelling = false;
        let session = self.session.clone().unwrap_or_default();
        self.request(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]}),
            Pending::Prompt,
        )
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
                Some(LiveCmd::Prompt { text }) => {
                    if st.ready {
                        let line = st.prompt(&text);
                        let _ = proc.write_line(line).await;
                    } else {
                        st.waiting.push_back(text);
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
                    report(&out, Out::Ev(Ev::Notice { level: "warn", code: "no_question", text: format!("{name} is not waiting for an answer") })).await;
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
    st.request("session/new", json!({"cwd": opts.cwd, "mcpServers": []}), Pending::Session { loading: false })
}

fn handle(st: &mut State, line: &str, opts: &Opts, name: &str) -> (Vec<String>, Vec<Out>) {
    let (mut writes, mut outs) = (Vec::new(), Vec::new());
    let Some(msg) = jsonrpc::parse(line) else { return (writes, outs) };
    match msg {
        Incoming::Response { id, result } => match (st.pending.remove(&id), result) {
            (Some(Pending::Initialize), Ok(r)) => {
                st.can_load = r["agentCapabilities"]["loadSession"].as_bool() == Some(true);
                match opts.resume.clone() {
                    Some(s) if st.can_load => {
                        st.loading = true;
                        st.session = Some(s.clone());
                        writes.push(st.request(
                            "session/load",
                            json!({"sessionId": s, "cwd": opts.cwd, "mcpServers": []}),
                            Pending::Session { loading: true },
                        ));
                    }
                    Some(_) => {
                        outs.push(Out::Ev(Ev::Notice {
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
            (Some(Pending::Session { loading }), Ok(r)) => {
                st.loading = false;
                if !loading {
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
                    writes.push(st.request(
                        "session/set_model",
                        json!({"sessionId": session, "modelId": model}),
                        Pending::Other,
                    ));
                }
                st.ready = true;
                outs.push(Out::Session(session));
                while let Some(text) = st.waiting.pop_front() {
                    writes.push(st.prompt(&text));
                }
            }
            (Some(Pending::Session { loading: true }), Err(e)) => {
                st.loading = false;
                st.session = None;
                outs.push(Out::Ev(Ev::Notice {
                    level: "warn",
                    code: "context_lost",
                    text: format!("{name} could not continue the earlier conversation ({e}); a new one starts"),
                }));
                writes.push(session_new(st, opts));
            }
            (Some(Pending::Session { loading: false }), Err(e)) => {
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
                outs.push(Out::Ev(Ev::Notice { level: "warn", code: "rpc_error", text: e }))
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
            if method == "session/update" && !st.loading && params["sessionId"].as_str() == st.session.as_deref() {
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
            st.text.push_str(&text);
            outs.push(Out::Ev(Ev::Message { id: format!("t{}", st.turn), text, delta: true }));
        }
        "agent_thought_chunk" => {
            outs.push(Out::Ev(Ev::Thought {
                id: format!("t{}r", st.turn),
                text: chunk_text(&u["content"]),
                delta: true,
            }));
        }
        "tool_call" => outs.push(Out::Ev(Ev::ToolCall {
            call_id: u["toolCallId"].as_str().unwrap_or("").to_string(),
            title: cut(u["title"].as_str().unwrap_or("tool"), 400).0,
            kind: kind(u["kind"].as_str()),
            status: status(u["status"].as_str()).unwrap_or(ToolStatus::Pending),
            input: u.get("rawInput").filter(|v| !v.is_null()).map(preview_value),
            locations: locations(u),
            content: contents(&u["content"]).0,
        })),
        "tool_call_update" => {
            let (content, truncated) = contents(&u["content"]);
            outs.push(Out::Ev(Ev::ToolUpdate {
                call_id: u["toolCallId"].as_str().unwrap_or("").to_string(),
                status: status(u["status"].as_str()),
                content,
                truncated,
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
        st.waiting.push_back("hi".into());
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
        let p = st.prompt("go");
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
            matches!(&o[0], Out::TurnEnd { status: TurnStatus::Completed, final_text: Some(t), .. } if t == "Done"),
            "{o:?}"
        );
        let (w, _) = handle(&mut st, r#"{"id":"f","method":"fs/read_text_file","params":{}}"#, &opts(None), "A");
        assert!(w[0].contains("-32601"));
    }
}
