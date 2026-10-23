//! Sessions end to end with a stand-in for Claude Code: a shell script that speaks just enough of
//! its stream-json protocol (Unix only).

#![cfg(unix)]

use std::{os::unix::fs::PermissionsExt, time::Duration};

use super::*;

/// Answers each message with "echo"; a message containing SLOW runs until interrupted, or until a
/// message joins it (a steer, answered "steered"); one containing LATE takes two seconds without
/// reading, so a message sent meanwhile runs as the next turn. Every message it takes is echoed
/// back (`--replay-user-messages`).
///
/// The control protocol as measured with Claude Code 2.1.308: it names the session when asked
/// (`generate_session_title`), switches modes (`set_permission_mode`, then a `status` line), and
/// asks with `can_use_tool`: ASK asks two questions (AskUserQuestion), PLAN proposes a plan
/// (ExitPlanMode), PERMIT wants to run a command (with a rule to allow it for the session). The
/// turn then ends with what the answer said: `allowed <answers>` or `denied <message>`. EDIT edits
/// a file and its result carries a `structuredPatch`.
const FAKE_CLAUDE: &str = r###"#!/bin/sh
if [ "$1" = "--version" ]; then echo "2.1.0 (Claude Code)"; exit 0; fi
MODE=$(printf '%s ' "$@" | sed -n 's/.*--permission-mode \([A-Za-z]*\).*/\1/p')
echo "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"fake-session\",\"skills\":[\"tidy\"],\"permissionMode\":\"$MODE\"}"
REPLAY='{"type":"user","isReplay":true,"message":{"role":"user","content":[{"type":"text","text":"(replay)"}]}}'
SLOW=""
field() { printf '%s' "$line" | sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p"; }
while IFS= read -r line; do
  case "$line" in
    *'"subtype":"initialize"'*)
      echo '{"type":"control_response","response":{"subtype":"success","request_id":"ar-init","response":{"commands":[{"name":"review","description":"Review the changes","argumentHint":"<pr>"},{"name":"tidy","description":"A skill"}]}}}'
      continue ;;
    *'"subtype":"generate_session_title"'*)
      echo "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$(field request_id)\",\"response\":{\"title\":\"Fake title\"}}}"
      continue ;;
    *'"subtype":"set_permission_mode"'*)
      MODE=$(field mode)
      echo "{\"type\":\"control_response\",\"response\":{\"subtype\":\"success\",\"request_id\":\"$(field request_id)\",\"response\":{\"mode\":\"$MODE\"}}}"
      echo "{\"type\":\"system\",\"subtype\":\"status\",\"status\":null,\"permissionMode\":\"$MODE\"}"
      continue ;;
    *'"type":"control_response"'*)
      # The answer to a can_use_tool: the turn ends with what it said.
      ANSWERS=$(printf '%s' "$line" | sed -n 's/.*"answers":\({[^}]*}\).*/\1/p' | tr -d '"')
      case "$line" in
        *'"behavior":"allow"'*) SAID="allowed $ANSWERS$(printf '%s' "$line" | grep -o '"destination":"[a-z]*"' | tr -d '"')" ;;
        *) SAID="denied $(field message)" ;;
      esac
      echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"$SAID\"}"
      continue ;;
    *control_request*)
      [ -n "$SLOW" ] && kill "$SLOW" 2>/dev/null
      SLOW=""
      echo '{"type":"result","subtype":"error_during_execution","is_error":true}'
      continue ;;
  esac
  echo "$REPLAY"
  if [ -n "$SLOW" ]; then
    kill "$SLOW" 2>/dev/null
    SLOW=""
    echo '{"type":"assistant","message":{"id":"st","model":"m","usage":{"input_tokens":10,"cache_read_input_tokens":90,"output_tokens":5},"content":[{"type":"text","text":"steered"}]}}'
    echo '{"type":"result","subtype":"success","is_error":false,"result":"steered","modelUsage":{"m[1m]":{"contextWindow":1000}}}'
    continue
  fi
  case "$line" in
    *SLOW*)
      sleep 600 & SLOW=$!
      echo '{"type":"assistant","message":{"id":"s","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"sleep 600"}}]}}' ;;
    *LATE*)
      echo '{"type":"assistant","message":{"id":"l","content":[{"type":"text","text":"late"}]}}'
      sleep 2
      echo '{"type":"result","subtype":"success","is_error":false,"result":"late"}' ;;
    *ASK*)
      echo '{"type":"control_request","request_id":"cu-ask","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[{"question":"Which config?","header":"Config","multiSelect":false,"options":[{"label":"debug"},{"label":"release"}]},{"question":"Which targets?","header":"Targets","multiSelect":true,"options":[{"label":"mac"},{"label":"win"}]}]},"tool_use_id":"tu-ask","requires_user_interaction":true}}' ;;
    *PLAN*)
      echo '{"type":"control_request","request_id":"cu-plan","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","input":{"plan":"## Plan: build it"},"tool_use_id":"tu-plan","requires_user_interaction":true}}' ;;
    *PERMIT*)
      echo '{"type":"assistant","message":{"id":"pm","content":[{"type":"tool_use","id":"tu-bash","name":"Bash","input":{"command":"cargo build"}}]}}'
      echo '{"type":"control_request","request_id":"cu-bash","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo build"},"permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"cargo build"}],"behavior":"allow","destination":"localSettings"}],"decision_reason":"This command requires approval","tool_use_id":"tu-bash"}}' ;;
    *EDIT*)
      echo '{"type":"assistant","message":{"id":"ed","content":[{"type":"tool_use","id":"tu-edit","name":"Edit","input":{"file_path":"/w/t.py","old_string":"b = 2","new_string":"b = 20"}}]}}'
      echo '{"type":"user","message":{"role":"user","content":[{"tool_use_id":"tu-edit","type":"tool_result","content":"updated"}]},"tool_use_result":{"filePath":"/w/t.py","oldString":"b = 2","newString":"b = 20","structuredPatch":[{"oldStart":1,"oldLines":3,"newStart":1,"newLines":3,"lines":[" a = 1","-b = 2","+b = 20"," c = 3"]}]}}'
      echo '{"type":"result","subtype":"success","is_error":false,"result":"edited"}' ;;
    *)
      echo '{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"echo"}]}}'
      echo '{"type":"result","subtype":"success","is_error":false,"result":"ok"}' ;;
  esac
done
"###;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    work: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let fake = root.join("fake-claude");
    std::fs::write(&fake, FAKE_CLAUDE).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cfg = format!(
        "[[agent]]\nid = \"fake\"\nbase = \"claude\"\ncommand = \"{0}\"\n\n\
         [[agent]]\nid = \"fake-nosteer\"\nbase = \"claude\"\ncommand = \"{0}\"\nsteer = false\n\n\
         [[agent]]\nid = \"fake-codex\"\nbase = \"codex\"\ncommand = \"{0}\"\n",
        fake.display()
    );
    std::fs::write(root.join("chat.toml"), cfg).unwrap();
    std::fs::create_dir_all(root.join("chat")).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    Fixture { _dir: dir, root, work }
}

fn manager(f: &Fixture) -> (ChatManager, mpsc::UnboundedReceiver<Value>) {
    ChatManager::new(f.root.join("chat"), f.root.join("chat.toml"))
}

fn creator() -> Peer {
    Peer { name: "laptop".into(), principal: Some("eid:laptop".into()), user_id: None, level: None, revoked: false }
}

fn message(text: &str) -> Input {
    Input {
        id: format!("u-{text}"),
        text: text.into(),
        by: By { device: "laptop".into(), principal: None },
        images: Vec::new(),
    }
}

fn input(text: &str) -> Cmd {
    Cmd::Input(message(text))
}

fn steer(text: &str) -> Cmd {
    Cmd::Steer(message(text))
}

/// Waits for an event matching `pred` on `rx`, returning it.
async fn wait_for(rx: &mut broadcast::Receiver<Arc<str>>, pred: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let line = tokio::time::timeout_at(deadline, rx.recv()).await.expect("event in time").expect("open");
        let v: Value = serde_json::from_str(&line).unwrap();
        if pred(&v) {
            return v;
        }
    }
}

#[tokio::test]
async fn a_session_runs_turns_queues_interrupts_and_ends() {
    let f = fixture();
    let (m, mut audit) = manager(&f);
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None, None).await.unwrap();
    let (_, _, mut rx) = chat.follow(0);
    chat.send(input("hello"));
    let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert_eq!((end["status"].as_str(), end["final_text"].as_str()), (Some("completed"), Some("ok")));
    assert_eq!(chat.meta().proto_session.as_deref(), Some("fake-session"));

    // The second message runs in the same process; the same message sent again (a controller
    // that lost its connection) does not run twice.
    let pid = chat.meta().pid;
    chat.send(input("again"));
    chat.send(input("again"));
    wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert_eq!(chat.meta().pid, pid);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(chat.meta().turns, 2);

    // A long turn, a message queued behind it, then an interrupt.
    chat.send(input("SLOW"));
    wait_for(&mut rx, |v| v["t"] == "tool_call").await;
    chat.send(input("queued"));
    let st = wait_for(&mut rx, |v| v["t"] == "state" && v["queued"].as_array().is_some_and(|q| !q.is_empty())).await;
    assert_eq!(st["queued"][0]["text"], "queued");
    chat.send(Cmd::Interrupt { then: None });
    let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert_eq!(end["status"], "interrupted");
    // The queued message goes out next.
    let user = wait_for(&mut rx, |v| v["t"] == "user").await;
    assert_eq!(user["text"], "queued");
    wait_for(&mut rx, |v| v["t"] == "turn_end").await;

    chat.send(Cmd::End { reason: "done".into() });
    wait_for(&mut rx, |v| v["t"] == "state" && v["state"] == "ended").await;
    assert_eq!(chat.state(), State::Ended);
    assert!(chat.meta().pid.is_none());

    // The audit has the session and its turns, never the text.
    let mut kinds = Vec::new();
    while let Ok(v) = audit.try_recv() {
        assert!(!v.to_string().contains("hello"), "{v}");
        kinds.push(v["kind"].as_str().unwrap().to_string());
    }
    assert_eq!(kinds.iter().filter(|k| *k == "chat_turn").count(), 4, "{kinds:?}");
    assert!(kinds.contains(&"chat_session".to_string()));

    // A viewer that comes back gets everything after what it saw.
    let (lines, upto, _) = chat.follow(3);
    assert_eq!(line_seq(&lines[0]), Some(4));
    assert_eq!(line_seq(lines.last().unwrap()), Some(upto));

    // Reopened by a new host: ended and numbered on.
    drop(chat);
    let (m2, _) = manager(&f);
    let again = m2.list(None);
    assert_eq!(again[0]["state"], "ended");
    assert_eq!(again[0]["seq"].as_u64(), Some(upto));
}

#[tokio::test]
async fn a_stopped_process_parks_and_the_next_message_resumes() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat =
        m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, Some("first".into()), None).await.unwrap();
    let (_, _, mut rx) = chat.follow(0);
    wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    chat.send(Cmd::Park { reason: "idle".into() });
    wait_for(&mut rx, |v| v["t"] == "state" && v["state"] == "parked").await;
    assert!(chat.meta().pid.is_none());
    chat.send(input("second"));
    let n = wait_for(&mut rx, |v| v["t"] == "notice").await;
    assert_eq!(n["code"], "resumed");
    let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert_eq!(end["status"], "completed");
}

#[tokio::test]
async fn starting_is_refused_when_switched_off_or_the_folder_is_missing() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    assert!(m.start(creator(), "fake", "/nonexistent/folder", None, false, None, None).await.is_err());
    assert!(m.start(creator(), "fake", "relative", None, false, None, None).await.is_err());
    assert!(m.start(creator(), "nope", f.work.to_str().unwrap(), None, false, None, None).await.is_err());
    assert!(
        m.start(creator(), "fake", f.work.to_str().unwrap(), Some("bad model".into()), false, None, None)
            .await
            .is_err()
    );
    m.set_enabled(false).unwrap();
    let e = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None, None).await.err().expect("refused");
    assert!(e.to_string().contains("switched off"), "{e}");
    // Persisted for the next host.
    let (m2, _) = manager(&f);
    assert!(!m2.enabled());
}

#[tokio::test]
async fn devices_that_lose_admin_lose_their_sessions() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat =
        m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, Some("hi".into()), None).await.unwrap();
    let (_, _, mut rx) = chat.follow(0);
    wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    m.enforce(|_| true);
    assert_ne!(chat.state(), State::Ended);
    m.enforce(|p| p.name != "laptop");
    wait_for(&mut rx, |v| v["t"] == "state" && v["state"] == "ended").await;
    assert!(chat.meta().end_reason.unwrap().contains("admin"));
}

#[tokio::test]
async fn a_restarted_host_stops_what_the_previous_one_left_running() {
    let f = fixture();
    // A session whose agent was mid-turn when the host died, its process still running.
    let mut child = std::process::Command::new("/bin/sh").args(["-c", "sleep 600 & wait"]).spawn().unwrap();
    let pid = child.id();
    std::thread::sleep(Duration::from_millis(200));
    let start = crate::tools::jobs::process_start_time(pid).unwrap();
    let dir = f.root.join("chat").join("corphan");
    std::fs::create_dir_all(&dir).unwrap();
    let meta = json!({
        "id": "corphan", "runtime": "fake", "name": "Fake", "cwd": f.work, "by": {"device": "laptop"},
        "started_at": 1, "state": "running", "pid": pid, "pid_start": start, "last_activity": 1,
    });
    std::fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
    std::fs::write(dir.join("events.jsonl"), "{\"seq\":1,\"ts\":1,\"turn\":1,\"t\":\"turn_start\"}\n").unwrap();
    let (m, mut audit) = manager(&f);
    let status = child.wait().unwrap();
    assert!(!status.success(), "the orphan was killed");
    let chat = m.get("corphan").unwrap();
    assert_eq!(chat.state(), State::Parked);
    let (lines, _, _) = chat.follow(1);
    assert!(lines[0].contains("turn_end") && lines[0].contains("restarted"), "{lines:?}");
    let reaped = audit.try_recv().unwrap();
    assert_eq!(reaped["action"], "orphan_reaped");
}

#[tokio::test]
async fn a_broken_settings_file_switches_agents_off() {
    let f = fixture();
    std::fs::write(f.root.join("chat.toml"), "enabled = false\n[[agent]]\nid = \"claude\"\nbase = \"claude\"\n")
        .unwrap();
    let (m, _audit) = manager(&f);
    assert!(!m.enabled());
    let e =
        m.start(creator(), "claude", f.work.to_str().unwrap(), None, false, None, None).await.err().expect("refused");
    assert!(e.to_string().contains("chat.toml"), "{e}");
    assert!(m.runtimes(false).await["host"]["config_error"].as_str().is_some_and(|s| s.contains("built-in")));
    // Not overwritten by a switch.
    assert!(m.set_enabled(true).is_err());
    assert!(std::fs::read_to_string(f.root.join("chat.toml")).unwrap().contains("enabled = false"));
}

#[tokio::test]
async fn a_steer_joins_the_running_turn() {
    let f = fixture();
    let (m, mut audit) = manager(&f);
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None, None).await.unwrap();
    let (_, _, mut rx) = chat.follow(0);
    chat.send(input("SLOW"));
    let start = wait_for(&mut rx, |v| v["t"] == "turn_start").await;
    wait_for(&mut rx, |v| v["t"] == "tool_call").await;
    chat.send(steer("change of plan"));
    let user = wait_for(&mut rx, |v| v["t"] == "user").await;
    assert_eq!((user["text"].as_str(), user["steer"].as_bool()), (Some("change of plan"), Some(true)));
    assert_eq!(user["turn"], start["turn"], "numbered with the running turn");
    let usage = wait_for(&mut rx, |v| v["t"] == "usage").await;
    assert_eq!((usage["context_used"].as_u64(), usage["context_window"].as_u64()), (Some(105), Some(1000)));
    let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert_eq!((end["status"].as_str(), end["final_text"].as_str()), (Some("completed"), Some("steered")));
    assert_eq!(end["turn"], start["turn"]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(chat.meta().turns, 1, "no turn of its own");
    let turns: Vec<Value> = std::iter::from_fn(|| audit.try_recv().ok()).filter(|v| v["kind"] == "chat_turn").collect();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["steers"], 1);
}

#[tokio::test]
async fn a_steer_the_turn_did_not_read_runs_next_without_being_sent_again() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None, None).await.unwrap();
    let (_, _, mut rx) = chat.follow(0);
    chat.send(input("LATE"));
    wait_for(&mut rx, |v| v["t"] == "message").await;
    chat.send(steer("one more thing"));
    let n = wait_for(&mut rx, |v| v["t"] == "notice").await;
    assert_eq!(n["code"], "steer_queued");
    let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert_eq!(end["final_text"], "late");
    let user = wait_for(&mut rx, |v| v["t"] == "user").await;
    assert_eq!((user["text"].as_str(), user.get("steer")), (Some("one more thing"), None));
    let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert_eq!(end["final_text"], "ok");
    // Sent once: had it been written again, the agent would have run a third turn.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(chat.meta().turns, 2);
    assert_eq!(chat.state(), State::Idle);
}

#[tokio::test]
async fn a_steer_with_no_turn_running_starts_one() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None, None).await.unwrap();
    let (_, _, mut rx) = chat.follow(0);
    chat.send(steer("hello"));
    let n = wait_for(&mut rx, |v| v["t"] == "notice").await;
    assert_eq!(n["code"], "steer_queued");
    let user = wait_for(&mut rx, |v| v["t"] == "user").await;
    assert!(user.get("steer").is_none());
    let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert_eq!(end["final_text"], "ok");
}

#[tokio::test]
async fn an_agent_that_cannot_steer_queues_the_message() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let caps = &m.runtimes(true).await["runtimes"];
    let row = |id: &str| caps.as_array().unwrap().iter().find(|r| r["id"] == id).cloned().unwrap();
    assert_eq!(
        (row("fake")["caps"]["steer"].as_bool(), row("fake-nosteer")["caps"]["steer"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(row("opencode")["caps"]["steer"], false);
    let chat = m.start(creator(), "fake-nosteer", f.work.to_str().unwrap(), None, false, None, None).await.unwrap();
    let (_, _, mut rx) = chat.follow(0);
    chat.send(input("SLOW"));
    wait_for(&mut rx, |v| v["t"] == "tool_call").await;
    chat.send(steer("later please"));
    let n = wait_for(&mut rx, |v| v["t"] == "notice").await;
    assert_eq!(n["code"], "steer_queued");
    let st = wait_for(&mut rx, |v| v["t"] == "state" && v["queued"].as_array().is_some_and(|q| !q.is_empty())).await;
    assert_eq!(st["queued"][0]["text"], "later please");
    chat.send(Cmd::Interrupt { then: None });
    let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert_eq!(end["status"], "interrupted");
    let user = wait_for(&mut rx, |v| v["t"] == "user").await;
    assert_eq!(user["text"], "later please");
    assert!(user.get("steer").is_none());
    wait_for(&mut rx, |v| v["t"] == "turn_end").await;
}

#[tokio::test]
async fn previews_read_only_files_the_session_is_about() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None, None).await.unwrap();
    std::fs::write(f.work.join("report.html"), "<h1>hi</h1>").unwrap();
    std::fs::write(f.work.join("notes.txt"), "x").unwrap();
    let outside = f.root.join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("chart.png"), b"\x89PNG").unwrap();
    std::fs::write(outside.join("secret.md"), "s").unwrap();
    std::os::unix::fs::symlink(outside.join("secret.md"), f.work.join("link.md")).unwrap();

    // Inside the folder, by a relative path; the kind comes from the extension.
    let p = chat.previewable("report.html").unwrap();
    assert_eq!((p.kind, p.size), ("html", 11));
    // Not a previewable kind, even inside the folder.
    assert!(chat.previewable("notes.txt").unwrap_err().to_string().contains("cannot be previewed"));
    // Outside and not named by the agent; a link out of the folder counts as outside.
    let chart = outside.join("chart.png").display().to_string();
    assert!(chat.previewable(&chart).unwrap_err().to_string().contains("outside"));
    assert!(chat.previewable("link.md").unwrap_err().to_string().contains("outside"));
    // A person naming it does not make it the agent's.
    chat.emit(Ev::User {
        attachments: vec![],
        input_id: "u".into(),
        by: By { device: "laptop".into(), principal: None },
        text: chart.clone(),
        steer: false,
    });
    assert!(chat.previewable(&chart).is_err());
    // Once the agent wrote it (a tool call's location), it can be previewed.
    chat.emit(Ev::ToolCall {
        call_id: "w".into(),
        parent: None,
        title: "Write".into(),
        kind: event::ToolKind::Edit,
        status: event::ToolStatus::Completed,
        input: None,
        locations: vec![chart.clone()],
        content: vec![],
    });
    assert_eq!(chat.previewable(&chart).unwrap().kind, "image");
    // Too big.
    std::fs::write(f.work.join("big.png"), vec![0u8; (preview::MAX_PREVIEW + 1) as usize]).unwrap();
    assert!(chat.previewable("big.png").unwrap_err().to_string().contains("at most"));
    chat.send(Cmd::End { reason: "done".into() });
}

#[tokio::test]
async fn the_agents_commands_are_kept_and_offered_to_new_sessions() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat =
        m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, Some("first".into()), None).await.unwrap();
    let (_, _, mut rx) = chat.follow(0);
    // `initialize` lists the commands; the first `init` says which are skills.
    let ev = wait_for(&mut rx, |v| v["t"] == "commands" && v["commands"][1]["kind"] == "skill").await;
    assert_eq!(
        ev["commands"][0],
        json!({"name": "review", "description": "Review the changes", "kind": "command", "insert": "/review", "hint": "<pr>"})
    );
    wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert_eq!(chat.meta().commands.len(), 2);
    // A restart of the agent sends the same list: not written again.
    chat.send(Cmd::Park { reason: "idle".into() });
    wait_for(&mut rx, |v| v["t"] == "state" && v["state"] == "parked").await;
    chat.send(input("second"));
    let mut lists = 0;
    loop {
        let v = wait_for(&mut rx, |v| v["t"] == "commands" || v["t"] == "turn_end").await;
        if v["t"] == "turn_end" {
            break;
        }
        lists += 1;
    }
    assert_eq!(lists, 0);
    // A new session in the same folder offers them before its agent has started.
    let other = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None, None).await.unwrap();
    assert_eq!(m.commands_for(&other).len(), 2);
    assert!(m.commands_for(&chat).iter().any(|c| c.name == "tidy" && c.kind == "skill"));
}

#[tokio::test]
async fn an_image_reaches_the_agent_and_the_transcript_keeps_only_its_name() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None, None).await.unwrap();
    assert!(m.takes_images(&chat));
    let (_, _, mut rx) = chat.follow(0);
    let mut msg = message("look");
    msg.images.push(Image::parse("shot.png", "image/png", "iVBORw0KGgo=").unwrap());
    chat.send(Cmd::Input(msg));
    let user = wait_for(&mut rx, |v| v["t"] == "user").await;
    assert_eq!(user["attachments"], json!([{"name": "shot.png", "mime": "image/png", "bytes": 8}]));
    wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    let (lines, _, _) = chat.follow(0);
    assert!(!lines.iter().any(|l| l.contains("iVBORw0KGgo")), "the image's bytes stay out of the transcript");
}

fn person(device: &str) -> By {
    By { device: device.into(), principal: Some(format!("eid:{device}")) }
}

#[tokio::test]
async fn a_permission_waits_for_the_first_answer_and_can_hold_for_the_session() {
    let f = fixture();
    let (m, mut audit) = manager(&f);
    let chat =
        m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None, Some("default".into())).await.unwrap();
    let (_, _, mut rx) = chat.follow(0);
    chat.send(input("PERMIT"));
    let p = wait_for(&mut rx, |v| v["t"] == "permission").await;
    assert_eq!((p["tool"].as_str(), p["title"].as_str()), (Some("Bash"), Some("Bash cargo build")));
    assert_eq!((p["suggestions"].clone(), p["call_id"].as_str()), (json!(["session"]), Some("tu-bash")));
    let rid = p["rid"].as_str().unwrap().to_string();
    assert_eq!(chat.meta().pending, Some(Pending::Permission));
    assert_eq!(chat.describe()["pending"], "permission");
    // Two devices answer: the first takes it, the second finds it gone (and a question's answer
    // never takes a permission).
    assert_eq!(chat.claim(&rid, &[Pending::Question]), None);
    assert_eq!(chat.claim(&rid, &[Pending::Permission, Pending::Plan]), Some(Pending::Permission));
    assert_eq!(chat.claim(&rid, &[Pending::Permission, Pending::Plan]), None);
    assert_eq!(chat.meta().pending, None);
    chat.send(Cmd::Permit {
        rid: rid.clone(),
        decision: Decision::Session,
        message: None,
        mode: None,
        by: person("phone"),
    });
    let r = wait_for(&mut rx, |v| v["t"] == "resolved").await;
    assert_eq!(
        (r["id"].as_str(), r["outcome"].as_str(), r["by"]["device"].as_str()),
        (Some(rid.as_str()), Some("allowed_session"), Some("phone"))
    );
    // The agent was told to allow it for this session only.
    let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert_eq!(end["final_text"], "allowed destination:session");
    let mut audited = false;
    while let Ok(v) = audit.try_recv() {
        if v["kind"] == "chat_permission" {
            assert_eq!((v["outcome"].as_str(), v["by"].as_str()), (Some("allowed_session"), Some("phone")));
            audited = true;
        }
    }
    assert!(audited);

    // No with a reason: the model reads it.
    chat.send(input("PERMIT again"));
    let p = wait_for(&mut rx, |v| v["t"] == "permission").await;
    let rid = p["rid"].as_str().unwrap().to_string();
    chat.claim(&rid, &[Pending::Permission]).unwrap();
    chat.send(Cmd::Permit {
        rid,
        decision: Decision::Deny,
        message: Some("not now".into()),
        mode: None,
        by: person("laptop"),
    });
    let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert_eq!(end["final_text"], "denied not now");

    // Unanswered when the turn stops: settled as cancelled, nothing waits any more.
    chat.send(input("PERMIT once more"));
    let p = wait_for(&mut rx, |v| v["t"] == "permission").await;
    chat.send(Cmd::Interrupt { then: None });
    let r = wait_for(&mut rx, |v| v["t"] == "resolved").await;
    assert_eq!((r["id"].clone(), r["outcome"].as_str()), (p["rid"].clone(), Some("cancelled")));
    wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert!(!chat.is_open(p["rid"].as_str().unwrap()));
    assert_eq!(chat.meta().pending, None);
}

#[tokio::test]
async fn questions_of_several_parts_and_plans_are_answered_and_switch_the_mode() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat =
        m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None, Some("plan".into())).await.unwrap();
    assert_eq!(chat.describe()["permission_mode"], "plan");
    let (_, _, mut rx) = chat.follow(0);
    chat.send(input("ASK me"));
    let q = wait_for(&mut rx, |v| v["t"] == "question").await;
    assert_eq!((q["prompt"].as_str(), q["options"].as_array().map(Vec::len)), (Some("Which config?"), Some(2)));
    assert_eq!(
        (q["questions"][1]["header"].as_str(), q["questions"][1]["multi"].as_bool()),
        (Some("Targets"), Some(true))
    );
    let qid = q["qid"].as_str().unwrap().to_string();
    assert_eq!(chat.meta().pending, Some(Pending::Question));
    assert!(chat.claim(&qid, &[Pending::Question]).is_some());
    let answers: BTreeMap<String, String> =
        [("Which config?".into(), "release".into()), ("Which targets?".into(), "mac".into())].into();
    chat.send(Cmd::Answer { qid: qid.clone(), text: "release".into(), answers: Some(answers), by: person("laptop") });
    let r = wait_for(&mut rx, |v| v["t"] == "resolved").await;
    assert_eq!((r["id"].as_str(), r["outcome"].as_str()), (Some(qid.as_str()), Some("answered")));
    let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
    assert_eq!(end["final_text"], "allowed {Which config?:release,Which targets?:mac}");

    // A plan approved to go on accepting edits: the mode follows.
    chat.send(input("PLAN it"));
    let p = wait_for(&mut rx, |v| v["t"] == "plan_review").await;
    assert_eq!(p["plan"], "## Plan: build it");
    let rid = p["rid"].as_str().unwrap().to_string();
    assert_eq!(chat.meta().pending, Some(Pending::Plan));
    chat.claim(&rid, &[Pending::Permission, Pending::Plan]).unwrap();
    chat.send(Cmd::Permit {
        rid,
        decision: Decision::Once,
        message: None,
        mode: Some("acceptEdits".into()),
        by: person("laptop"),
    });
    // (The switch is confirmed after the turn's end here.)
    wait_for(&mut rx, |v| v["t"] == "mode" && v["mode"] == "acceptEdits").await;
    assert_eq!(chat.meta().permission_mode.as_deref(), Some("acceptEdits"));

    // Switched by a person, the agent confirms it.
    chat.send(Cmd::SetMode { mode: "default".into(), by: person("laptop") });
    wait_for(&mut rx, |v| v["t"] == "mode" && v["mode"] == "default").await;
    chat.send(Cmd::SetMode { mode: "yolo".into(), by: person("laptop") });
    let n = wait_for(&mut rx, |v| v["t"] == "notice").await;
    assert_eq!(n["code"], "mode_rejected");
}

#[tokio::test]
async fn titles_come_from_the_agent_unless_a_person_named_the_session() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None, None).await.unwrap();
    let (_, _, mut rx) = chat.follow(0);
    chat.send(input("Fix the build please"));
    // The title may come before the turn's end or after it (the stand-in answers after).
    let t = wait_for(&mut rx, |v| v["t"] == "title").await;
    assert_eq!((t["title"].as_str(), t["source"].as_str()), (Some("Fake title"), Some("agent")));
    for _ in 0..50 {
        if chat.meta().last_status.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let d = chat.describe();
    assert_eq!((d["title"].as_str(), d["first_text"].as_str()), (Some("Fake title"), Some("Fix the build please")));
    assert_eq!(
        (d["last_status"].as_str(), d["permission_mode"].as_str()),
        (Some("completed"), Some("bypassPermissions"))
    );
    assert_eq!(d["last_seq"], d["seq"]);
    // Renamed, pinned, archived.
    let d = m.update(&chat.id, Some("My build".into()), Some(true), Some(true), &creator()).unwrap();
    assert_eq!(
        (d["title"].as_str(), d["title_source"].as_str(), d["pinned"].as_bool()),
        (Some("My build"), Some("user"), Some(true))
    );
    let t = wait_for(&mut rx, |v| v["t"] == "title").await;
    assert_eq!(t["source"], "user");
    assert!(m.list(Some(false)).iter().all(|s| s["id"] != chat.id.as_str()));
    assert!(m.list(Some(true)).iter().any(|s| s["id"] == chat.id.as_str()));
    let d = m.update(&chat.id, None, None, Some(false), &creator()).unwrap();
    assert_eq!(d["title"], "My build");
    assert!(m.update(&chat.id, Some("x".repeat(201)), None, None, &creator()).is_err());
    // Empty: back to none (the first message shows).
    let d = m.update(&chat.id, Some(" ".into()), None, None, &creator()).unwrap();
    assert!(d.get("title").is_none() && d.get("title_source").is_none());
}

#[tokio::test]
async fn an_edit_carries_its_line_numbers_and_modes_are_claude_codes_alone() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None, None).await.unwrap();
    let (_, _, mut rx) = chat.follow(0);
    chat.send(input("EDIT"));
    let u = wait_for(&mut rx, |v| v["t"] == "tool_update").await;
    assert_eq!(u["content"][0]["hunks"][0]["lines"], json!([" a = 1", "-b = 2", "+b = 20", " c = 3"]));
    assert_eq!(u["content"][0]["hunks"][0]["old_start"], 1);
    wait_for(&mut rx, |v| v["t"] == "turn_end").await;

    let codex = m
        .start(creator(), "fake-codex", f.work.to_str().unwrap(), None, false, None, Some("plan".into()))
        .await
        .unwrap();
    assert!(codex.describe().get("permission_mode").is_none(), "only Claude Code has modes");
    let (_, _, mut rx) = codex.follow(0);
    codex.send(Cmd::SetMode { mode: "plan".into(), by: person("laptop") });
    let n = wait_for(&mut rx, |v| v["t"] == "notice").await;
    assert_eq!(n["code"], "mode_rejected");
    // Nothing is asked: an answer finds nothing to take.
    assert!(codex.claim("q1", &[Pending::Question]).is_none());
    let bad = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None, Some("yolo".into())).await;
    assert!(bad.is_err());
}

#[tokio::test]
async fn an_older_hosts_session_gains_its_first_message_and_last_outcome() {
    let f = fixture();
    let dir = f.root.join("chat").join("cold");
    std::fs::create_dir_all(&dir).unwrap();
    let meta = json!({
        "id": "cold", "runtime": "fake", "name": "Fake", "cwd": f.work, "by": {"device": "laptop"},
        "started_at": 1, "state": "parked", "turns": 2, "last_activity": 1,
    });
    std::fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
    let by = json!({"device": "laptop"});
    let events = [
        json!({"seq": 1, "ts": 1, "turn": 0, "t": "user", "input_id": "u1", "by": by, "text": "  first ask  "}),
        json!({"seq": 2, "ts": 1, "turn": 1, "t": "turn_start"}),
        json!({"seq": 3, "ts": 1, "turn": 1, "t": "turn_end", "status": "completed", "duration_ms": 1}),
        json!({"seq": 4, "ts": 1, "turn": 1, "t": "user", "input_id": "u2", "by": by, "text": "second"}),
        json!({"seq": 5, "ts": 1, "turn": 2, "t": "turn_end", "status": "failed", "duration_ms": 1, "reason": "401"}),
    ];
    // Written as the host writes them: compact, the envelope first.
    let text: String = events.iter().map(|e| format!("{}\n", serde_json::to_string(e).unwrap())).collect();
    std::fs::write(dir.join("events.jsonl"), text).unwrap();
    let (m, _audit) = manager(&f);
    let d = m.get("cold").unwrap().describe();
    assert_eq!((d["first_text"].as_str(), d["last_status"].as_str()), (Some("first ask"), Some("failed")));
    assert_eq!((d["last_reason"].as_str(), d["permission_mode"].as_str()), (Some("401"), Some("bypassPermissions")));
    assert_eq!((d["pinned"].as_bool(), d["archived"].as_bool()), (Some(false), Some(false)));
    // Written back: the next host finds them in meta.json.
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(saved["first_text"], "first ask");
}

/// The next event of type `what` (or the turn's end, which comes instead when it is not asked),
/// printing what passes.
async fn wait(rx: &mut broadcast::Receiver<Arc<str>>, what: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let line = tokio::time::timeout_at(deadline, rx.recv()).await.expect(what).expect("open");
        eprintln!("{}", line.chars().take(300).collect::<String>());
        let v: Value = serde_json::from_str(&line).unwrap();
        if v["t"] == what || v["t"] == "turn_end" {
            return v;
        }
    }
}

/// The permission flows against a real Claude Code (its sign-in and a little of its quota):
///
///   AR_REAL_CLAUDE=$(command -v claude) cargo test -p ar-host --lib real_claude -- --ignored --nocapture
#[tokio::test]
#[ignore = "needs a signed-in Claude Code in AR_REAL_CLAUDE"]
async fn real_claude_asks_titles_and_switches_modes() {
    let Ok(program) = std::env::var("AR_REAL_CLAUDE") else { return };
    let f = fixture();
    std::fs::write(
        f.root.join("chat.toml"),
        format!("[[agent]]\nid = \"real\"\nbase = \"claude\"\ncommand = {}\n", json!(program)),
    )
    .unwrap();
    let (m, _audit) = manager(&f);
    let work = f.work.to_str().unwrap();
    let chat =
        m.start(creator(), "real", work, Some("haiku".into()), true, None, Some("default".into())).await.unwrap();
    let (_, _, mut rx) = chat.follow(0);
    chat.send(input(
        "Use the AskUserQuestion tool to ask me ONE question: which color I prefer, options red and blue. After I \
         answer, reply with exactly: YOU CHOSE <color>.",
    ));
    let q = wait(&mut rx, "question").await;
    assert_eq!(q["t"], "question", "{q}");
    let qid = q["qid"].as_str().unwrap().to_string();
    let question = q["questions"][0]["question"].as_str().unwrap().to_string();
    chat.claim(&qid, &[Pending::Question]).unwrap();
    let answers = [(question, "blue".to_string())].into();
    chat.send(Cmd::Answer { qid, text: "blue".into(), answers: Some(answers), by: person("laptop") });
    let end = wait(&mut rx, "turn_end").await;
    assert!(end["final_text"].as_str().is_some_and(|t| t.to_lowercase().contains("blue")), "{end}");
    assert!(chat.meta().title.is_some(), "the agent named the session: {:?}", chat.meta());

    chat.send(input(
        "Run exactly this shell command with the Bash tool and report its output: \
         curl -s -o /dev/null -w %{http_code} https://example.com",
    ));
    let p = wait(&mut rx, "permission").await;
    assert_eq!((p["t"].as_str(), p["tool"].as_str()), (Some("permission"), Some("Bash")), "{p}");
    let rid = p["rid"].as_str().unwrap().to_string();
    chat.claim(&rid, &[Pending::Permission]).unwrap();
    chat.send(Cmd::Permit { rid, decision: Decision::Session, message: None, mode: None, by: person("laptop") });
    let end = wait(&mut rx, "turn_end").await;
    assert_eq!(end["status"], "completed", "{end}");
    assert!(!f.work.join(".claude/settings.local.json").exists(), "a session rule is not written to the project");

    chat.send(Cmd::SetMode { mode: "plan".into(), by: person("laptop") });
    let mode = wait(&mut rx, "mode").await;
    assert_eq!(mode["mode"], "plan", "{mode}");
    chat.send(Cmd::End { reason: "done".into() });
}
