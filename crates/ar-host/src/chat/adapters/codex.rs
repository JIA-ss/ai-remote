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

use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{LiveCmd, exit_reason, report};
use crate::chat::{
    event::{Ev, Out, PREVIEW, PlanEntry, ToolContent, ToolKind, ToolStatus, TurnStatus, cut, preview_value},
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
    Other,
}

#[derive(Default)]
struct State {
    next: u64,
    pending: HashMap<u64, Pending>,
    thread: Option<String>,
    turn: Option<String>,
    ready: bool,
    waiting: VecDeque<String>,
    streamed: HashSet<String>,
    final_text: Option<String>,
    last_error: Option<String>,
    /// A question request waiting for answers: its JSON-RPC id, the question ids and the answers so far.
    asking: Option<(Value, Vec<String>, HashMap<String, String>)>,
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
                Some(LiveCmd::Prompt { text }) => {
                    if st.ready {
                        let line = turn_start(&mut st, &text);
                        let _ = proc.write_line(line).await;
                    } else {
                        st.waiting.push_back(text);
                    }
                }
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

fn turn_start(st: &mut State, text: &str) -> String {
    st.final_text = None;
    st.last_error = None;
    st.streamed.clear();
    let thread = st.thread.clone().unwrap_or_default();
    st.request(
        "turn/start",
        json!({"threadId": thread, "input": [{"type": "text", "text": text, "text_elements": []}]}),
        Pending::TurnStart,
    )
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
                    while let Some(text) = st.waiting.pop_front() {
                        writes.push(turn_start(st, &text));
                    }
                } else {
                    outs.push(Out::Exited("Codex did not return a thread".into()));
                }
            }
            (Some(Pending::Thread { resumed: true }), Err(e)) => {
                outs.push(Out::Ev(Ev::Notice {
                    level: "warn",
                    code: "context_lost",
                    text: format!("Codex could not continue the earlier conversation ({e}); a new one starts"),
                }));
                writes.push(thread_start(st, opts));
            }
            (Some(Pending::Thread { resumed: false }), Err(e)) => {
                outs.push(Out::Exited(format!("Codex could not start a thread: {e}")))
            }
            (Some(Pending::TurnStart), Ok(r)) => st.turn = r["turn"]["id"].as_str().map(str::to_string),
            (Some(Pending::TurnStart), Err(e)) => {
                outs.push(Out::TurnEnd { status: TurnStatus::Failed, final_text: None, reason: Some(e) })
            }
            (Some(Pending::Other), Err(e)) => {
                outs.push(Out::Ev(Ev::Notice { level: "warn", code: "rpc_error", text: e }))
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
                outs.push(Out::Ev(ev));
            }
        }
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
            let changes: Vec<&Value> = item["changes"].as_array().into_iter().flatten().collect();
            let paths: Vec<&str> = changes.iter().filter_map(|c| c["path"].as_str()).collect();
            let content = changes
                .iter()
                .take(20)
                .map(|c| ToolContent::Text {
                    text: cut(
                        &format!("{}\n{}", c["path"].as_str().unwrap_or(""), c["diff"].as_str().unwrap_or("")),
                        PREVIEW,
                    )
                    .0,
                })
                .collect();
            (ToolKind::Edit, format!("edit {}", paths.join(", ")), content, None)
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
        "commandExecution" | "fileChange" | "mcpToolCall" | "dynamicToolCall" | "webSearch" => {
            let status = match item["status"].as_str().unwrap_or("completed") {
                "failed" | "declined" => ToolStatus::Failed,
                "inProgress" => ToolStatus::InProgress,
                _ => ToolStatus::Completed,
            };
            let mut text = item["aggregatedOutput"].as_str().unwrap_or("").to_string();
            if let Some(code) = item["exitCode"].as_i64()
                && code != 0
            {
                text.push_str(&format!("\n(exit code {code})"));
            }
            if text.is_empty()
                && let Some(r) = item.get("result").filter(|r| !r.is_null())
            {
                text = super::content_text(&r["content"]);
            }
            if text.is_empty()
                && let Some(e) = item["error"]["message"].as_str()
            {
                text = e.to_string();
            }
            let (text, truncated) = cut(text.trim(), PREVIEW);
            outs.push(Out::Ev(Ev::ToolUpdate {
                call_id: id,
                status: Some(status),
                content: if text.is_empty() { vec![] } else { vec![ToolContent::Text { text }] },
                truncated,
            }));
        }
        _ => {}
    }
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
        st.waiting.push_back("hi".into());
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
}
