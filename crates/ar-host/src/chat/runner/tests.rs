//! The runner protocol: the handshake, and a host that dies mid-turn while its agent keeps running
//! in the runner, then comes back and carries on with the same process (Unix: the stand-in agent
//! is a shell script; the transport is the same local socket code as the Windows pipe).

use super::*;

#[tokio::test]
async fn the_handshake_needs_the_same_key_on_both_sides() {
    let key = new_key();
    let (a, b) = tokio::io::duplex(4096);
    let (mut host, mut runner) = (Wire::new(a), Wire::new(b));
    let (h, r) = tokio::join!(handshake_host(&mut host, &key), handshake_runner(&mut runner, &key));
    assert_eq!(h.unwrap(), std::process::id() as u64);
    r.unwrap();

    // A runner without the key is refused; a host without it is not believed.
    let (a, b) = tokio::io::duplex(4096);
    let (mut host, mut runner) = (Wire::new(a), Wire::new(b));
    let other = new_key();
    let (h, r) = tokio::join!(handshake_host(&mut host, &key), handshake_runner(&mut runner, &other));
    assert!(h.is_err() && r.is_err(), "{h:?} {r:?}");
}

/// A host that came back asks for the live agents right after the handshake; that request may
/// arrive with the handshake's last line and must not be lost with the handshake's read buffer.
#[tokio::test]
async fn a_request_right_after_the_handshake_is_answered() {
    let key = new_key();
    let (a, b) = tokio::io::duplex(1 << 16);
    let runner = tokio::spawn(async move {
        let mut wire = Wire::new(b);
        handshake_runner(&mut wire, &key).await.unwrap();
        Arc::new(Runner::default()).serve(wire).await
    });
    let mut host = Wire::new(a);
    handshake_host(&mut host, &key).await.unwrap();
    host.write(&json!({"id": 1, "op": "lives"})).await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), host.read()).await.expect("an answer").unwrap().unwrap();
    assert_eq!(reply["id"], 1, "{reply}");
    assert_eq!(reply["ok"], json!([]), "{reply}");
    drop(host);
    runner.await.unwrap();
}

#[test]
fn keys_are_kept_for_the_next_host() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chat-runner.key");
    let k = load_or_create_key(&path).unwrap();
    assert_eq!(load_or_create_key(&path).unwrap(), k);
    assert_eq!(parse_key(&key_hex(&k)).unwrap(), k);
    assert!(parse_key("abcd").is_err());
}

/// The same tests on both transports: a Unix socket, and on Windows the named pipe the service
/// mode uses (the stand-in agent is a shell script there, a PowerShell script here).
mod restart {
    use std::sync::atomic::AtomicUsize;

    use ar_net::Peer;
    use tokio::sync::broadcast;

    use super::*;
    use crate::chat::{Backend, ChatManager, Cmd, Input, State, event::By};

    /// Claude Code's protocol, just enough: LATE answers a second later, a message with an image
    /// answers "image", HOLD takes the next line into the same turn (as Claude Code does with a
    /// message written during a turn, echoing both), anything else answers at once with the
    /// process id, so the test can tell one process from another.
    #[cfg(unix)]
    const FAKE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "2.1.0 (Claude Code)"; exit 0; fi
echo '{"type":"system","subtype":"init","session_id":"fake-session"}'
while IFS= read -r line; do
  case "$line" in
    *'"subtype":"initialize"'*)
      echo '{"type":"control_response","response":{"subtype":"success","request_id":"ar-init","response":{"commands":[]}}}' ;;
    *control_request*) echo '{"type":"result","subtype":"error_during_execution","is_error":true}' ;;
    *'"type":"image"'*)
      echo '{"type":"result","subtype":"success","is_error":false,"result":"image"}' ;;
    *HOLD*)
      echo '{"type":"user","isReplay":true,"message":{"role":"user","content":[{"type":"text","text":"hold"}]}}'
      IFS= read -r more
      echo '{"type":"user","isReplay":true,"message":{"role":"user","content":[{"type":"text","text":"more"}]}}'
      echo '{"type":"result","subtype":"success","is_error":false,"result":"steered"}' ;;
    *LATE*) sleep 1
      echo '{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"late"}]}}'
      echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"pid $$\"}" ;;
    *)
      echo '{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"echo"}]}}'
      echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"pid $$\"}" ;;
  esac
done
"#;

    #[cfg(windows)]
    const FAKE: &str = r#"
function Say($s) { [Console]::Out.WriteLine($s); [Console]::Out.Flush() }
if ($args -contains '--version') { Say '2.1.0 (Claude Code)'; exit 0 }
Say '{"type":"system","subtype":"init","session_id":"fake-session"}'
while ($null -ne ($line = [Console]::In.ReadLine())) {
  if ($line -like '*"subtype":"initialize"*') {
    Say '{"type":"control_response","response":{"subtype":"success","request_id":"ar-init","response":{"commands":[]}}}'
  } elseif ($line -like '*control_request*') {
    Say '{"type":"result","subtype":"error_during_execution","is_error":true}'
  } elseif ($line -like '*"type":"image"*') {
    Say '{"type":"result","subtype":"success","is_error":false,"result":"image"}'
  } elseif ($line -like '*HOLD*') {
    Say '{"type":"user","isReplay":true,"message":{"role":"user","content":[{"type":"text","text":"hold"}]}}'
    $more = [Console]::In.ReadLine()
    Say '{"type":"user","isReplay":true,"message":{"role":"user","content":[{"type":"text","text":"more"}]}}'
    Say '{"type":"result","subtype":"success","is_error":false,"result":"steered"}'
  } elseif ($line -like '*LATE*') {
    Start-Sleep -Seconds 1
    Say '{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"late"}]}}'
    Say ('{"type":"result","subtype":"success","is_error":false,"result":"pid ' + $PID + '"}')
  } else {
    Say '{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"echo"}]}}'
    Say ('{"type":"result","subtype":"success","is_error":false,"result":"pid ' + $PID + '"}')
  }
}
"#;

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        work: PathBuf,
        endpoint: PathBuf,
        key: [u8; 32],
    }

    #[cfg(unix)]
    fn fixture() -> Fixture {
        use std::os::unix::fs::PermissionsExt;
        // Under /tmp: macOS's temporary directory is too long for a socket path.
        let dir = tempfile::Builder::new().prefix("ar-runner").tempdir_in("/tmp").unwrap();
        let root = dir.path().to_path_buf();
        let fake = root.join("fake-claude");
        std::fs::write(&fake, FAKE).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = format!("[[agent]]\nid = \"fake\"\nbase = \"claude\"\ncommand = \"{}\"\n", fake.display());
        let endpoint = root.join("ep").join("runner.sock");
        finish(dir, cfg, endpoint)
    }

    #[cfg(windows)]
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake-claude.ps1");
        std::fs::write(&fake, FAKE).unwrap();
        // TOML literal strings keep the backslashes of the paths.
        let cfg = format!(
            "[[agent]]\nid = \"fake\"\nbase = \"claude\"\ncommand = '{}'\nargs = ['-NoProfile', '-NonInteractive', \
             '-ExecutionPolicy', 'Bypass', '-File', '{}']\n",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            fake.display()
        );
        let endpoint = PathBuf::from(format!(r"\\.\pipe\ai-remote-runner-test-{}", &crate::chat::random_hex()[..12]));
        finish(dir, cfg, endpoint)
    }

    fn finish(dir: tempfile::TempDir, cfg: String, endpoint: PathBuf) -> Fixture {
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("chat.toml"), cfg).unwrap();
        std::fs::create_dir_all(root.join("chat")).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        Fixture { _dir: dir, root, work, endpoint, key: new_key() }
    }

    /// A host: the endpoint, the link and a chat manager on it, all on `rt`, so dropping `rt` is a
    /// host that died. The runner it starts runs on `runner_rt` (it is another process really).
    async fn host(
        f: &Fixture,
        rt: &tokio::runtime::Runtime,
        runner_rt: tokio::runtime::Handle,
        spawned: Arc<AtomicUsize>,
    ) -> ChatManager {
        let (endpoint, key, chat_dir, cfg) = (f.endpoint.clone(), f.key, f.root.join("chat"), f.root.join("chat.toml"));
        rt.spawn(async move {
            let mut listener = loop {
                // The previous host's socket goes away with its runtime.
                match ar_net::ipc::Listener::bind_shared(&endpoint, &[]) {
                    Ok(l) => break l,
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            };
            let ep = endpoint.clone();
            let link = RunnerLink::new(Box::new(move || {
                spawned.fetch_add(1, Ordering::SeqCst);
                runner_rt.spawn(run(ep.clone(), key, Duration::from_secs(30), Duration::from_secs(30)));
                Ok(())
            }));
            let serving = link.clone();
            tokio::spawn(async move {
                while let Ok(conn) = listener.accept().await {
                    let link = serving.clone();
                    tokio::spawn(async move {
                        let mut wire = Wire::new(conn.into_raw().0);
                        if handshake_host(&mut wire, &key).await.is_ok() {
                            link.serve(wire).await;
                        }
                    });
                }
            });
            ChatManager::with_backend(chat_dir, cfg, Backend::Runner(link)).0
        })
        .await
        .unwrap()
    }

    fn creator() -> Peer {
        Peer { name: "laptop".into(), principal: Some("eid:laptop".into()), user_id: None, level: None, revoked: false }
    }

    fn input(text: &str) -> Cmd {
        Cmd::Input(Input {
            id: format!("u-{text}"),
            text: text.into(),
            by: By { device: "laptop".into(), principal: None },
            images: Vec::new(),
        })
    }

    async fn wait_for(rx: &mut broadcast::Receiver<Arc<str>>, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let line = tokio::time::timeout_at(deadline, rx.recv()).await.expect("event in time").expect("open");
            let v: Value = serde_json::from_str(&line).unwrap();
            if pred(&v) {
                return v;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agent_outlives_its_host_and_its_session_goes_on() {
        let f = fixture();
        let spawned = Arc::new(AtomicUsize::new(0));
        let here = tokio::runtime::Handle::current();

        // First host: a turn, then a slow one that the host dies in the middle of.
        let rt1 = tokio::runtime::Runtime::new().unwrap();
        let m1 = host(&f, &rt1, here.clone(), spawned.clone()).await;
        let runtimes = m1.runtimes(false).await;
        let fake = runtimes["runtimes"].as_array().unwrap().iter().find(|r| r["id"] == "fake").cloned().unwrap();
        assert_eq!(fake["verdict"], "ok", "{fake}");
        assert_eq!(fake["version"], "2.1.0", "{fake}");
        // `~` is the user's home as the runner sees it; a relative folder is refused there.
        assert!(m1.start(creator(), "fake", "relative", None, false, None).await.is_err());
        let chat = m1.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).await.unwrap();
        let id = chat.id.clone();
        let (_, _, mut rx) = chat.follow(0);
        chat.send(input("hello"));
        let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
        let first_pid = end["final_text"].as_str().unwrap().to_string();
        assert!(first_pid.starts_with("pid "), "{end}");
        chat.send(input("LATE"));
        wait_for(&mut rx, |v| v["t"] == "turn_start").await;
        // Long enough for the message to leave this host; the agent answers only after a second.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let seen = chat.describe()["seq"].as_u64().unwrap();
        drop((chat, rx, m1));
        rt1.shutdown_background();

        // The agent answers while no host is there; the runner keeps the answer.
        tokio::time::sleep(Duration::from_millis(1500)).await;

        // Second host: takes the agent back, the answer arrives, the session goes on in the same
        // process.
        let rt2 = tokio::runtime::Runtime::new().unwrap();
        let m2 = host(&f, &rt2, here.clone(), spawned.clone()).await;
        let chat = m2.get(&id).unwrap();
        let (old, _, mut rx) = chat.follow(seen);
        let mut all: Vec<Value> = old.iter().map(|l| serde_json::from_str(l).unwrap()).collect();
        let end = loop {
            if let Some(e) = all.iter().find(|v| v["t"] == "turn_end").cloned() {
                break e;
            }
            let line = match tokio::time::timeout(Duration::from_secs(15), rx.recv()).await {
                Ok(l) => l.unwrap(),
                Err(_) => panic!("no turn end after the restart: {all:?} meta {:?}", chat.meta()),
            };
            all.push(serde_json::from_str(&line).unwrap());
        };
        assert_eq!(end["status"], "completed", "{all:?}");
        assert_eq!(end["final_text"].as_str(), Some(first_pid.as_str()), "the same agent process answered");
        assert!(all.iter().any(|v| v["t"] == "notice" && v["code"] == "reattached"), "{all:?}");
        assert!(all.iter().any(|v| v["t"] == "message" && v["text"] == "late"), "{all:?}");
        // Numbering goes on from where the first host stopped, without gaps.
        let seqs: Vec<u64> = all.iter().map(|v| v["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs[0], seen + 1, "{seqs:?}");
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");

        chat.send(input("again"));
        let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
        assert_eq!(end["final_text"].as_str(), Some(first_pid.as_str()));
        wait_for(&mut rx, |v| v["t"] == "state" && v["state"] == "idle").await;
        assert_eq!(chat.state(), State::Idle);
        assert_eq!(spawned.load(Ordering::SeqCst), 1, "the runner was started once and found again");

        // A clean stop ends the runner and its agent.
        m2.close().await;
        let pid: u32 = first_pid.trim_start_matches("pid ").parse().unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let mut sys = sysinfo::System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let alive =
            sys.process(sysinfo::Pid::from_u32(pid)).is_some_and(|p| p.status() != sysinfo::ProcessStatus::Zombie);
        assert!(!alive, "the agent stopped with the runner");
        drop((chat, m2));
        rt2.shutdown_background();
    }

    /// A message written while a turn runs crosses the pipe as a steer, and the agent's answer that
    /// it took it into the running turn comes back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_steer_crosses_the_pipe_and_joins_the_running_turn() {
        let f = fixture();
        let spawned = Arc::new(AtomicUsize::new(0));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let m = host(&f, &rt, tokio::runtime::Handle::current(), spawned).await;
        let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).await.unwrap();
        let (_, _, mut rx) = chat.follow(0);
        chat.send(input("HOLD"));
        wait_for(&mut rx, |v| v["t"] == "state" && v["state"] == "running").await;
        chat.send(Cmd::Steer(Input {
            id: "u-more".into(),
            text: "and this too".into(),
            by: By { device: "laptop".into(), principal: None },
            images: Vec::new(),
        }));
        let joined = wait_for(&mut rx, |v| v["t"] == "user" && v["input_id"] == "u-more").await;
        assert_eq!(joined["steer"], true, "{joined}");
        let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
        assert_eq!(end["final_text"], "steered", "{end}");
        assert_eq!(chat.meta().turns, 1, "the steer did not start a turn of its own");
        m.close().await;
        drop((chat, m));
        rt.shutdown_background();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_image_crosses_the_pipe_to_an_agent_in_the_runner() {
        let f = fixture();
        let spawned = Arc::new(AtomicUsize::new(0));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let m = host(&f, &rt, tokio::runtime::Handle::current(), spawned).await;
        let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).await.unwrap();
        let (_, _, mut rx) = chat.follow(0);
        let image = crate::chat::Image {
            name: "dot.png".into(),
            mime: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
            bytes: 8,
        };
        chat.send(Cmd::Input(Input {
            id: "u-image".into(),
            text: "what is this".into(),
            by: By { device: "laptop".into(), principal: None },
            images: vec![image],
        }));
        let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
        assert_eq!(end["final_text"], "image", "{end}");
        m.close().await;
        drop((chat, m));
        rt.shutdown_background();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_preview_is_read_by_the_runner() {
        let f = fixture();
        let spawned = Arc::new(AtomicUsize::new(0));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let m = host(&f, &rt, tokio::runtime::Handle::current(), spawned).await;
        let chat = m.start(creator(), "fake", f.work.to_str().unwrap(), None, false, None).await.unwrap();
        std::fs::write(f.work.join("report.html"), "<h1>hi</h1>").unwrap();
        let found = chat.previewable("report.html").unwrap();
        assert_eq!(m.read_preview(&found.path).await.unwrap(), b"<h1>hi</h1>");
        // The runner checks again: a file that is gone is not read.
        std::fs::remove_file(f.work.join("report.html")).unwrap();
        assert!(m.read_preview(&found.path).await.is_err());
        m.close().await;
        drop((chat, m));
        rt.shutdown_background();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_runner_that_is_gone_leaves_a_parked_session() {
        let f = fixture();
        let spawned = Arc::new(AtomicUsize::new(0));
        // A session whose agent ran in a runner that no longer exists.
        let dir = f.root.join("chat").join("cgone");
        std::fs::create_dir_all(&dir).unwrap();
        let meta = json!({
            "id": "cgone", "runtime": "fake", "name": "Fake", "cwd": f.work, "by": {"device": "laptop"},
            "started_at": 1, "state": "running", "pid": 999_999, "pid_start": 1, "last_activity": 1,
            "runner_live": "cgone-1", "runner_seq": 3,
            "turn": {"input_id": "u1", "by": {"device": "laptop"}, "input_sha256": "x", "input_chars": 1, "started_at": 1},
        });
        std::fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
        std::fs::write(dir.join("events.jsonl"), "{\"seq\":1,\"ts\":1,\"turn\":1,\"t\":\"turn_start\"}\n").unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let m = host(&f, &rt, tokio::runtime::Handle::current(), spawned.clone()).await;
        let chat = m.get("cgone").unwrap();
        let (_, _, mut rx) = chat.follow(1);
        let end = wait_for(&mut rx, |v| v["t"] == "turn_end").await;
        assert!(end["reason"].as_str().unwrap().contains("restarted"), "{end}");
        wait_for(&mut rx, |v| v["t"] == "state" && v["state"] == "parked").await;
        assert!(chat.meta().runner_live.is_none() && chat.meta().turn.is_none());
        // Nothing was started just to look.
        assert_eq!(spawned.load(Ordering::SeqCst), 0);
        drop((chat, m));
        rt.shutdown_background();
    }
}
