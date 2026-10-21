#!/usr/bin/env python3
"""End-to-end check of coding-agent chats on one machine: an isolated host and controller pair over real
QUIC (admin level, chat tools allowed without asking), a stand-in agent that speaks Claude Code's
stream-json protocol, and the `ai-remote chat` commands against it: list, run, send --wait, a queued
message, an interrupt, attach with replay, end, the host's on/off switch, the audit and a lowered level.

  python tools/chat-smoke.py [--cli target/debug/ai-remote] [--real codex=/path/to/codex --model M]

--real adds one real agent as a custom entry (same protocol as the built-in one) and hands it a small
task; it uses the agent's own sign-in and spends a little of its quota.
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXE = ".exe" if os.name == "nt" else ""
parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--cli", type=Path, default=ROOT / f"target/debug/ai-remote{EXE}")
parser.add_argument("--real", action="append", default=[], help="base=path of a real agent, e.g. codex=/usr/local/bin/codex")
parser.add_argument("--model", help="model for the real agent")
options = parser.parse_args()

failures = []


def check(name, ok, detail=""):
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + (f"  ({detail})" if detail else ""))
    if not ok:
        failures.append(name)


# Speaks just enough of Claude Code's stream-json: echoes each message; one containing SLOW runs until
# interrupted.
FAKE_AGENT = r'''
import json, sys, threading
def out(v):
    sys.stdout.write(json.dumps(v) + "\n"); sys.stdout.flush()
if "--version" in sys.argv:
    print("2.1.0 (Claude Code)"); sys.exit(0)
out({"type": "system", "subtype": "init", "session_id": "fake-session"})
slow = None
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("type") == "control_request":
        if slow:
            slow.set(); slow = None
            out({"type": "result", "subtype": "error_during_execution", "is_error": True})
        continue
    text = msg["message"]["content"][0]["text"]
    if "SLOW" in text:
        slow = threading.Event()
        out({"type": "assistant", "message": {"id": "s", "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "sleep 600"}}]}})
        continue
    out({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "m"}}})
    for word in ("you ", "said: ", text):
        out({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": word}}})
    out({"type": "assistant", "message": {"id": "m", "content": [{"type": "text", "text": "you said: " + text}]}})
    out({"type": "result", "subtype": "success", "is_error": False, "result": "you said: " + text})
'''

with tempfile.TemporaryDirectory(prefix="arc-", dir="/tmp" if os.name != "nt" else None, ignore_cleanup_errors=True) as tmp:
    base = Path(tmp)
    host_home, ctl_home, work = base / "host", base / "controller", base / "work"
    work.mkdir()
    (host_home / "host").mkdir(parents=True)
    (host_home / "host" / "ai-remote.toml").write_text(
        '[host]\napproval_prompt = false\n\n[[rule]]\ntool = "chat_*"\naction = "allow"\n', encoding="utf-8")
    fake = base / "fake_agent.py"
    fake.write_text(FAKE_AGENT, encoding="utf-8")
    agents = [f'[[agent]]\nid = "fake"\nbase = "claude"\nname = "Fake agent"\ncommand = {json.dumps(sys.executable)}\n'
              f'args = ["-u", {json.dumps(str(fake))}]\n']
    for real in options.real:
        kind, path = real.split("=", 1)
        agents.append(f'[[agent]]\nid = "real-{kind}"\nbase = "{kind}"\ncommand = {json.dumps(path)}\n')
    (host_home / "host" / "chat.toml").write_text("\n".join(agents), encoding="utf-8")
    env = {k: v for k, v in os.environ.items() if k != "AI_REMOTE_HOST"}

    def cli(*args, home=ctl_home, timeout=120):
        return subprocess.run([str(options.cli), "--home", str(home), *map(str, args)], capture_output=True,
                              text=True, encoding="utf-8", errors="replace", timeout=timeout, env=env)

    log = open(base / "host.log", "w")
    host = subprocess.Popen([str(options.cli), "--home", str(host_home), "host", "--no-pair-code"],
                            stdout=log, stderr=log, env=env)
    try:
        ticket = None
        for _ in range(100):
            r = cli("--json", "pair-code", "--level", "admin", home=host_home)
            if r.returncode == 0:
                ticket = json.loads(r.stdout)["line"]
                break
            time.sleep(0.2)
        assert ticket, "host did not start"
        r = cli("pair", ticket, "--name", "self")
        check("pair (admin)", r.returncode == 0, r.stderr.strip()[-200:])

        r = cli("--json", "chat", "list")
        rows = {x["id"]: x for x in json.loads(r.stdout)["runtimes"]} if r.returncode == 0 else {}
        check("chat list finds the custom agent", rows.get("fake", {}).get("verdict") == "ok"
              and rows["fake"].get("version") == "2.1.0", json.dumps(rows.get("fake"))[:200] or r.stderr[-200:])
        check("chat list reports built-in agents", "claude" in rows and "codex" in rows)

        r = cli("chat", "run", "fake", "hello there", "--cwd", work)
        check("chat run prints the answer and exits 0", r.returncode == 0 and "you said: hello there" in r.stdout,
              f"code {r.returncode}: {r.stdout[-200:]} {r.stderr[-200:]}")

        r = cli("chat", "run", "fake", "first", "--cwd", work, "--keep")
        check("chat run --keep", r.returncode == 0)
        r = cli("--json", "chat", "sessions")
        sessions = json.loads(r.stdout)["sessions"] if r.returncode == 0 else []
        kept = next((s for s in sessions if s["state"] != "ended"), None)
        check("chat sessions lists the kept session", kept is not None and kept["turns"] == 1, json.dumps(sessions)[:300])
        sid = kept["id"] if kept else "none"

        r = cli("chat", "send", sid, "second", "--wait")
        check("chat send --wait answers on the same session", r.returncode == 0 and "you said: second" in r.stdout,
              f"code {r.returncode}: {r.stdout[-200:]} {r.stderr[-200:]}")

        r = cli("chat", "send", sid, "SLOW job")
        check("chat send without --wait returns", r.returncode == 0, r.stderr[-200:])
        r = cli("chat", "send", sid, "after slow")
        time.sleep(1.0)
        r = cli("chat", "interrupt", sid)
        check("chat interrupt", r.returncode == 0, r.stderr[-200:])
        deadline = time.time() + 20
        while time.time() < deadline:
            out = cli("--json", "chat", "sessions")
            s = next((x for x in json.loads(out.stdout)["sessions"] if x["id"] == sid), {})
            if s.get("turns") == 4 and s.get("state") == "idle":
                break
            time.sleep(0.5)
        check("the queued message runs after the interrupted turn", s.get("turns") == 4 and s.get("state") == "idle",
              json.dumps(s)[:200])

        # attach replays the conversation; with stdin closed it leaves at once.
        p = subprocess.run([str(options.cli), "--home", str(ctl_home), "--json", "chat", "attach", sid],
                           capture_output=True, text=True, timeout=30, env=env, stdin=subprocess.DEVNULL)
        events = [json.loads(l) for l in p.stdout.splitlines() if l.startswith("{")]
        ends = [e["status"] for e in events if e.get("t") == "turn_end"]
        check("attach replays every turn in order", ends == ["completed", "completed", "interrupted", "completed"], str(ends))
        users = [e["text"] for e in events if e.get("t") == "user"]
        check("replayed messages are the ones sent", users == ["first", "second", "SLOW job", "after slow"], str(users))

        r = cli("chat", "end", sid)
        check("chat end", r.returncode == 0, r.stderr[-200:])

        r = cli("chat", "disable", home=host_home)
        check("chat disable (on the host)", r.returncode == 0, r.stderr[-200:])
        r = cli("chat", "run", "fake", "x", "--cwd", work)
        check("a switched-off host refuses to start agents (exit 2)", r.returncode == 2 and "switched off" in r.stderr,
              f"code {r.returncode}: {r.stderr[-200:]}")
        r = cli("chat", "enable", home=host_home)
        check("chat enable", r.returncode == 0)

        audit = [json.loads(l) for l in (host_home / "host" / "audit.jsonl").read_text(encoding="utf-8").splitlines()]
        turns = [a for a in audit if a.get("kind") == "chat_turn"]
        check("each turn is audited", len(turns) == 5, str(len(turns)))
        check("the audit keeps hashes, not the conversation", not any("hello there" in json.dumps(a) for a in audit
                                                                      if a.get("kind", "").startswith("chat")))

        for real in options.real:
            kind = real.split("=", 1)[0]
            args = ["chat", "run", f"real-{kind}", "Reply with exactly the word PONG and nothing else.", "--cwd", work]
            if options.model:
                args += ["--model", options.model]
            t0 = time.time()
            r = cli(*args, timeout=300)
            check(f"real {kind}: answers a task", r.returncode == 0 and "PONG" in r.stdout,
                  f"{time.time() - t0:.1f} s, code {r.returncode}: {r.stdout[-200:]} {r.stderr[-300:]}")

        peers = json.loads(cli("--json", "peers", home=host_home).stdout)
        peer = (peers.get("peers") if isinstance(peers, dict) else peers)[0]["name"]
        r = cli("level", peer, "control", home=host_home)
        check("lower the controller to control", r.returncode == 0, r.stderr[-200:])
        time.sleep(1)
        r = cli("chat", "list")
        if r.returncode != 0 and "admin" not in r.stderr:
            r = cli("chat", "list")  # the level change cut the connection; the next call redials
        check("level control cannot see agents", r.returncode == 2 and "admin" in r.stderr, f"code {r.returncode}: {r.stderr[-200:]}")
    finally:
        cli("stop-host", home=host_home)
        try:
            host.wait(timeout=10)
        except subprocess.TimeoutExpired:
            host.kill()
        cli("stop-agent")
        log.close()
        if failures:
            print((base / "host.log").read_text(encoding="utf-8", errors="replace")[-3000:])

print(f"\n{len(failures)} failed" if failures else "\nall passed")
sys.exit(1 if failures else 0)
