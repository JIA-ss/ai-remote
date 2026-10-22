//! Sessions end to end with a stand-in for Claude Code: a shell script that speaks just enough of
//! its stream-json protocol (Unix only).

#![cfg(unix)]

use std::{os::unix::fs::PermissionsExt, time::Duration};

use super::*;

/// Answers each message with "echo"; a message containing SLOW runs until interrupted, or until a
/// message joins it (a steer, answered "steered"); one containing LATE takes two seconds without
/// reading, so a message sent meanwhile runs as the next turn. Every message it takes is echoed
/// back (`--replay-user-messages`).
const FAKE_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "2.1.0 (Claude Code)"; exit 0; fi
echo '{"type":"system","subtype":"init","session_id":"fake-session","skills":["tidy"]}'
REPLAY='{"type":"user","isReplay":true,"message":{"role":"user","content":[{"type":"text","text":"(replay)"}]}}'
SLOW=""
while IFS= read -r line; do
  case "$line" in
    *'"subtype":"initialize"'*)
      echo '{"type":"control_response","response":{"subtype":"success","request_id":"ar-init","response":{"commands":[{"name":"review","description":"Review the changes","argumentHint":"<pr>"},{"name":"tidy","description":"A skill"}]}}}'
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
    *)
      echo '{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"echo"}]}}'
      echo '{"type":"result","subtype":"success","is_error":false,"result":"ok"}' ;;
  esac
done
"#;

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
         [[agent]]\nid = \"fake-nosteer\"\nbase = \"claude\"\ncommand = \"{0}\"\nsteer = false\n",
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
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).await.unwrap();
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
    let again = m2.list();
    assert_eq!(again[0]["state"], "ended");
    assert_eq!(again[0]["seq"].as_u64(), Some(upto));
}

#[tokio::test]
async fn a_stopped_process_parks_and_the_next_message_resumes() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, Some("first".into())).await.unwrap();
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
    assert!(m.start(creator(), "fake", "/nonexistent/folder", None, false, None).await.is_err());
    assert!(m.start(creator(), "fake", "relative", None, false, None).await.is_err());
    assert!(m.start(creator(), "nope", f.work.to_str().unwrap(), None, false, None).await.is_err());
    assert!(m.start(creator(), "fake", f.work.to_str().unwrap(), Some("bad model".into()), false, None).await.is_err());
    m.set_enabled(false).unwrap();
    let e = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).await.err().expect("refused");
    assert!(e.to_string().contains("switched off"), "{e}");
    // Persisted for the next host.
    let (m2, _) = manager(&f);
    assert!(!m2.enabled());
}

#[tokio::test]
async fn devices_that_lose_admin_lose_their_sessions() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, Some("hi".into())).await.unwrap();
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
    let e = m.start(creator(), "claude", f.work.to_str().unwrap(), None, false, None).await.err().expect("refused");
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
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).await.unwrap();
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
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).await.unwrap();
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
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).await.unwrap();
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
    let chat = m.start(creator(), "fake-nosteer", f.work.to_str().unwrap(), None, false, None).await.unwrap();
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
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).await.unwrap();
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
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, Some("first".into())).await.unwrap();
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
    let other = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).await.unwrap();
    assert_eq!(m.commands_for(&other).len(), 2);
    assert!(m.commands_for(&chat).iter().any(|c| c.name == "tidy" && c.kind == "skill"));
}

#[tokio::test]
async fn an_image_reaches_the_agent_and_the_transcript_keeps_only_its_name() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).await.unwrap();
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
