#!/usr/bin/env python3
"""The desktop app's agent chat panel end to end, on a Mac: an isolated host with stand-in agents, the
debug desktop app on its own profile, and a script (tools/desktop-chat-e2e.js) that drives the real UI in
the app's webview (AI_REMOTE_SMOKE_SCRIPT, debug builds only). The chain is the real one: page → Tauri →
controller agent → QUIC → host → agent process.

The stand-ins speak Claude Code's stream-json: "Rich fake" answers a message containing RICH with
reasoning, a plan (TodoWrite), Read, a failing Bash, Grep, an Edit, a Markdown answer and context usage;
SLOW keeps a turn running until the next message (a steer) or Stop; FILES writes an HTML page, Markdown and a
PNG into the session's folder and a Markdown file outside it (named in a Write), for the file previews
(tools/desktop-chat-preview-e2e.js, run with E2E_SCRIPT=tools/desktop-chat-preview-e2e.js). "No-steer fake" is the same program
with `steer = false`. Neither asks questions (Claude's adapter does not carry them), so the question
card is not covered here.

Every home gets `[network] quiet = true` for the host and controller roles. The app window stays off
screen; screenshots land in --out.

  cargo build -p ai-remote && (cd apps/desktop && npx tauri build --debug --no-bundle)
  python3 tools/desktop-chat-e2e.py [--out /tmp/desktop-chat-e2e]

macOS (WKWebView snapshot) and Windows (PrintWindow; the webview is told to keep drawing off screen).
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXE = ".exe" if os.name == "nt" else ""
TMP = None if os.name == "nt" else "/tmp"
parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--cli", type=Path, default=ROOT / f"target/debug/ai-remote{EXE}")
parser.add_argument("--app", type=Path, default=ROOT / f"target/debug/ai-remote-desktop{EXE}")
parser.add_argument("--out", type=Path, default=Path(tempfile.gettempdir()) / "desktop-chat-e2e")
parser.add_argument("--verbose", action="store_true", help="print everything the app writes to stderr")
parser.add_argument("--secs", type=int, default=300, help="give up after this long")
parser.add_argument("--real", metavar="AGENT", help="drive a real agent installed on this Mac instead (its name as the "
                    "panel lists it, e.g. 'Claude Code'): a small file task and a steer; uses its sign-in and quota")
options = parser.parse_args()

RICH_AGENT = r'''
import json, sys, threading
def out(v):
    sys.stdout.write(json.dumps(v) + "\n"); sys.stdout.flush()
if "--version" in sys.argv:
    print("2.1.0 (Claude Code)"); sys.exit(0)
out({"type": "system", "subtype": "init", "session_id": "fake-session"})
OLD = "def quarter_end(year, q):\n    return date(year, q * 3, 31)\n"
ANSWER = """Fixed: the quarter end was always **day 31**.

### What changed
- `quarter_end` takes the month's last day from `calendar.monthrange`.
- Q2 and Q3 export again.

| Quarter | Before | Now |
|:--|:--|:--|
| Q2 | `ValueError` | ok |
| Q3 | `ValueError` | ok |

```sh
python export.py --quarter Q3
```
"""
def stream(mid, kind, text):
    out({"type": "stream_event", "event": {"type": "message_start", "message": {"id": mid}}})
    for i in range(0, len(text), 12):
        d = {"type": "thinking_delta", "thinking": text[i:i+12]} if kind == "thinking" else {"type": "text_delta", "text": text[i:i+12]}
        out({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": d}})
def tool(mid, calls, results):
    out({"type": "assistant", "message": {"id": mid, "model": "claude-fake", "content": calls,
         "usage": {"input_tokens": 1200, "cache_read_input_tokens": 40000, "output_tokens": 600}}})
    out({"type": "user", "message": {"content": results}})
def todo(*states):
    names = ["Reproduce the failure", "Use the month's real last day", "Run the tests"]
    return {"type": "tool_use", "id": "todo" + "".join(s[0] for s in states), "name": "TodoWrite",
            "input": {"todos": [{"content": n, "status": s, "activeForm": n} for n, s in zip(names, states)]}}
def rich():
    stream("r1", "thinking", "The quarter end is built with day 31; June and September have 30 days. Reproduce first.")
    tool("r1", [todo("in_progress", "pending", "pending"),
                {"type": "tool_use", "id": "read1", "name": "Read", "input": {"file_path": "/work/export.py"}},
                {"type": "tool_use", "id": "bash1", "name": "Bash", "input": {"command": "python export.py --quarter Q3"}}],
         [{"type": "tool_result", "tool_use_id": "read1", "content": OLD},
          {"type": "tool_result", "tool_use_id": "bash1", "is_error": True,
           "content": "Traceback (most recent call last):\nValueError: day is out of range for month\n(exit code 1)"}])
    tool("r2", [{"type": "tool_use", "id": "grep1", "name": "Grep", "input": {"pattern": "date\\(year, .*31\\)"}},
                {"type": "tool_use", "id": "edit1", "name": "Edit", "input": {"file_path": "/work/export.py",
                 "old_string": "    return date(year, q * 3, 31)",
                 "new_string": "    last = calendar.monthrange(year, q * 3)[1]\n    return date(year, q * 3, last)"}},
                todo("completed", "completed", "completed")],
         [{"type": "tool_result", "tool_use_id": "grep1", "content": "export.py:2:    return date(year, q * 3, 31)"},
          {"type": "tool_result", "tool_use_id": "edit1", "content": "ok"}])
    stream("r3", "text", ANSWER)
    out({"type": "assistant", "message": {"id": "r3", "model": "claude-fake", "content": [{"type": "text", "text": ANSWER}],
         "usage": {"input_tokens": 1200, "cache_read_input_tokens": 40000, "output_tokens": 600}}})
    out({"type": "result", "subtype": "success", "is_error": False, "result": "Fixed.",
         "usage": {"input_tokens": 1200, "cache_read_input_tokens": 40000, "output_tokens": 600},
         "modelUsage": {"claude-fake": {"contextWindow": 200000}}})
PAGE = """<!doctype html><html><head><title>Preview page</title>
<script>parent.postMessage("script ran", "*"); document.title = "script ran";</script></head>
<body style="font:15px system-ui;margin:24px"><h1 id="h">Quarterly report</h1><p>Static text renders.</p>
<img src="https://example.com/beacon.png" alt="remote image"></body></html>"""
NOTES = "# Notes\n\n| Quarter | Pages |\n| --- | ---: |\n| Q1 | 11 |\n\n- **bold** point\n"
def png(w, h):
    import struct, zlib
    rows = b"".join(b"\x00" + b"".join(bytes((x * 255 // w, y * 255 // h, 160)) for x in range(w)) for y in range(h))
    chunk = lambda k, d: struct.pack(">I", len(d)) + k + d + struct.pack(">I", zlib.crc32(k + d) & 0xffffffff)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) +
            chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))
def files():
    import os
    work = os.getcwd()
    out_dir = os.path.join(work, "out")
    outside = os.path.join(os.path.dirname(work), "outside")
    os.makedirs(out_dir, exist_ok=True); os.makedirs(outside, exist_ok=True)
    page, notes, chart = (os.path.join(out_dir, n) for n in ("report.html", "notes.md", "chart.png"))
    named, secret = os.path.join(outside, "named.md"), os.path.join(outside, "secret.md")
    open(page, "w").write(PAGE); open(notes, "w").write(NOTES); open(chart, "wb").write(png(64, 32))
    open(named, "w").write("# Named outside\n"); open(secret, "w").write("# Not named\n")
    tool("f1", [{"type": "tool_use", "id": "w1", "name": "Write", "input": {"file_path": page, "content": PAGE}},
                {"type": "tool_use", "id": "w2", "name": "Write", "input": {"file_path": notes, "content": NOTES}},
                {"type": "tool_use", "id": "w3", "name": "Write", "input": {"file_path": named, "content": "# Named outside\n"}},
                {"type": "tool_use", "id": "b1", "name": "Bash", "input": {"command": "python chart.py > out/chart.png"}}],
         [{"type": "tool_result", "tool_use_id": "w1", "content": "ok"}, {"type": "tool_result", "tool_use_id": "w2", "content": "ok"},
          {"type": "tool_result", "tool_use_id": "w3", "content": "ok"}, {"type": "tool_result", "tool_use_id": "b1", "content": "wrote out/chart.png"}])
    answer = "Done: the [report](out/report.html), notes in `out/notes.md` and a chart `out/chart.png`."
    out({"type": "assistant", "message": {"id": "f2", "content": [{"type": "text", "text": answer}]}})
    out({"type": "result", "subtype": "success", "is_error": False, "result": answer})
slow = None
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("type") == "control_request":
        if slow:
            slow.set(); slow = None
            out({"type": "result", "subtype": "error_during_execution", "is_error": True})
        continue
    if msg.get("type") != "user":
        continue
    text = msg["message"]["content"][0]["text"]
    out({"type": "user", "isReplay": True, "message": {"role": "user", "content": [{"type": "text", "text": text}]}})
    if slow:
        slow.set(); slow = None
        out({"type": "assistant", "message": {"id": "st", "content": [{"type": "text", "text": "steered: " + text}]}})
        out({"type": "result", "subtype": "success", "is_error": False, "result": "steered: " + text})
        continue
    if "SLOW" in text:
        slow = threading.Event()
        out({"type": "assistant", "message": {"id": "s", "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "sleep 600"}}]}})
        continue
    if "RICH" in text:
        rich(); continue
    if "FILES" in text:
        files(); continue
    out({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "m"}}})
    for word in ("you ", "said: ", text):
        out({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": word}}})
    out({"type": "assistant", "message": {"id": "m", "content": [{"type": "text", "text": "you said: " + text}]}})
    out({"type": "result", "subtype": "success", "is_error": False, "result": "you said: " + text})
'''

for exe in (options.cli, options.app):
    if not exe.exists():
        sys.exit(f"missing {exe}: build it first (see the docstring)")
options.out.mkdir(parents=True, exist_ok=True)
for old in options.out.glob("*.png"):
    old.unlink()

with tempfile.TemporaryDirectory(prefix="adc-", dir=TMP, **({"ignore_cleanup_errors": True} if sys.version_info >= (3, 10) else {})) as tmp:
    base = Path(tmp)
    # Not inside `base`: on Windows, Python 3.12's mkdtemp gives it an owner-only ACL, and WebView2's
    # sandboxed processes then cannot use a data folder there (0x80080005 at startup).
    webview = Path(tempfile.gettempdir()) / f"adc-webview-{os.getpid()}"
    host_home, desk, work = base / "host", base / "desk", base / "work"
    work.mkdir()
    quiet = "[network]\nquiet = true\n"
    (host_home / "host").mkdir(parents=True)
    (host_home / "host" / "ai-remote.toml").write_text(
        '[host]\napproval_prompt = false\n\n' + quiet + '\n[[rule]]\ntool = "chat_*"\naction = "allow"\n', encoding="utf-8")
    for home in (host_home, desk):
        for role in ("host", "controller"):
            (home / role).mkdir(parents=True, exist_ok=True)
            if not (home / role / "ai-remote.toml").exists():
                (home / role / "ai-remote.toml").write_text(quiet, encoding="utf-8")
    agent = base / "rich_agent.py"
    agent.write_text(RICH_AGENT, encoding="utf-8")
    entry = f'command = {json.dumps(sys.executable)}\nargs = ["-u", {json.dumps(str(agent))}]\n'
    (host_home / "host" / "chat.toml").write_text(
        f'[[agent]]\nid = "rich"\nbase = "claude"\nname = "Rich fake"\n{entry}\n'
        f'[[agent]]\nid = "nosteer"\nbase = "claude"\nname = "No-steer fake"\nsteer = false\n{entry}', encoding="utf-8")
    env = {k: v for k, v in os.environ.items() if k not in ("AI_REMOTE_HOST", "AI_REMOTE_HOME")}

    def cli(*args, home):
        return subprocess.run([str(options.cli), "--home", str(home), *map(str, args)], capture_output=True, text=True,
                              encoding="utf-8", errors="replace", timeout=120, env=env)

    log = open(base / "host.log", "w")
    host = subprocess.Popen([str(options.cli), "--home", str(host_home), "host", "--no-pair-code"], stdout=log, stderr=log, env=env)
    code = 1
    try:
        for _ in range(100):
            r = cli("--json", "pair-code", "--level", "admin", home=host_home)
            if r.returncode == 0:
                ticket = json.loads(r.stdout)["line"]
                break
            time.sleep(0.2)
        else:
            raise SystemExit("host did not start")
        doctor = cli("doctor", home=host_home).stdout
        network = next((l for l in doctor.splitlines() if l.split()[1:2] == ["network"]), "")
        assert network.split()[:1] == ["ok"] and "quiet" in network, "the test host is not in quiet mode:\n" + doctor
        # A new name each run: the debug app shares the installed app's webview storage (same bundle id),
        # where the panel remembers the last session per host name.
        assert cli("pair", ticket, "--name", f"e2e-{os.getpid()}", home=desk).returncode == 0, "the desktop profile did not pair"
        name = json.loads(cli("--json", "hosts", home=desk).stdout)[0]["name"]
        print(f"host {name!r}, desktop profile {desk}")

        script = base / "drive.js"
        source = ROOT / (os.environ.get("E2E_SCRIPT") or ("tools/desktop-chat-e2e-real.js" if options.real else "tools/desktop-chat-e2e.js"))
        script.write_text(source.read_text(encoding="utf-8")
                          .replace("__CONFIG__", json.dumps({"work": str(work), "host": name, "agent": options.real})), encoding="utf-8")
        app_env = {**env, "AI_REMOTE_HOME": str(desk), "AI_REMOTE_SMOKE": str(options.out / "zz-final.png"),
                   "AI_REMOTE_SMOKE_SECS": str(options.secs), "AI_REMOTE_SMOKE_SCRIPT": str(script),
                   "AI_REMOTE_SMOKE_QUERY": f"view=session&host={name}&panel=chat",
                   # Windows: keep the off-screen webview drawing, and a throwaway webview profile.
                   "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS": "--disable-features=CalculateNativeWinOcclusion",
                   "WEBVIEW2_USER_DATA_FOLDER": str(webview)}
        app = subprocess.Popen([str(options.app)], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
                               encoding="utf-8", errors="replace", env=app_env)
        passed = None
        for line in app.stderr:
            line = line.rstrip()
            if options.verbose:
                print("  |", line[:400])
            if line.startswith("[page e2e] "):
                text = line[len("[page e2e] "):]
                print(text)
                if text in ("all passed",) or re.fullmatch(r"\d+ failed", text):
                    passed = text == "all passed"
            elif line.startswith("[page ") and ("error" in line or "warn" in line):
                print("  app:", line[:300])
            elif line.startswith("[smoke]"):
                print("  " + line)
        print(f"  app exited with {app.wait(timeout=60)}")
        code = 0 if passed else 1
        if passed is None:
            print("the script did not finish")
    finally:
        cli("stop-agent", home=desk)
        cli("stop-host", home=host_home)
        try:
            host.wait(timeout=10)
        except subprocess.TimeoutExpired:
            host.kill()
        log.close()
        shutil.rmtree(webview, ignore_errors=True)
    if code:
        print((base / "host.log").read_text(encoding="utf-8", errors="replace")[-2000:])
    print(f"screenshots: {options.out}")
    sys.exit(code)
