//! Sessions end to end with a stand-in for Claude Code: a shell script that speaks just enough of
//! its stream-json protocol (Unix only).

#![cfg(unix)]

use std::{os::unix::fs::PermissionsExt, time::Duration};

use super::*;

/// Answers each message with "echo"; a message containing SLOW runs until interrupted.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "2.1.0 (Claude Code)"; exit 0; fi
echo '{"type":"system","subtype":"init","session_id":"fake-session"}'
SLOW=""
while IFS= read -r line; do
  case "$line" in
    *control_request*)
      [ -n "$SLOW" ] && kill "$SLOW" 2>/dev/null
      SLOW=""
      echo '{"type":"result","subtype":"error_during_execution","is_error":true}' ;;
    *SLOW*)
      sleep 600 & SLOW=$!
      echo '{"type":"assistant","message":{"id":"s","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"sleep 600"}}]}}' ;;
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
    let cfg = format!("[[agent]]\nid = \"fake\"\nbase = \"claude\"\ncommand = \"{}\"\n", fake.display());
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

fn input(text: &str) -> Cmd {
    Cmd::Input(Input {
        id: format!("u-{text}"),
        text: text.into(),
        by: By { device: "laptop".into(), principal: None },
    })
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
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).unwrap();
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
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, Some("first".into())).unwrap();
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
    assert!(m.start(creator(), "fake", "/nonexistent/folder", None, false, None).is_err());
    assert!(m.start(creator(), "fake", "relative", None, false, None).is_err());
    assert!(m.start(creator(), "nope", f.work.to_str().unwrap(), None, false, None).is_err());
    assert!(m.start(creator(), "fake", f.work.to_str().unwrap(), Some("bad model".into()), false, None).is_err());
    m.set_enabled(false).unwrap();
    let e = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).err().expect("refused");
    assert!(e.to_string().contains("switched off"), "{e}");
    // Persisted for the next host.
    let (m2, _) = manager(&f);
    assert!(!m2.enabled());
}

#[tokio::test]
async fn devices_that_lose_admin_lose_their_sessions() {
    let f = fixture();
    let (m, _audit) = manager(&f);
    let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, Some("hi".into())).unwrap();
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
