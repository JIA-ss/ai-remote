#!/usr/bin/env python3
"""Checks the live input channel (`app/ai-remote/input/1`) on one machine: an isolated host and
controller pair over real QUIC at level control, then a script talks to the controller agent's
local socket exactly as the desktop app does (`{"op":"input"}`), sends events and reads the host's
lines back. The host injects into THIS desktop: one pointer move to the screen's centre, a shift
press and release, and a clipboard round trip (the clipboard is restored afterwards). Refusals are
checked without anything being sent (a lock chord, a policy that denies the mouse tool).

  python3 tools/live-input-smoke.py [--cli target/debug/ai-remote]
"""
import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXE = ".exe" if os.name == "nt" else ""
parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--cli", type=Path, default=ROOT / f"target/debug/ai-remote{EXE}")
options = parser.parse_args()
if os.name == "nt":
    sys.exit("this check talks to the agent over a Unix socket; run it on macOS or Linux")

failures = []


def check(name, ok, detail=""):
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + (f"  ({detail})" if detail else ""))
    if not ok:
        failures.append(name)


class Agent:
    """One connection to the controller agent, upgraded to live input."""

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(str(path))
        self.sock.settimeout(10)
        self.buf = b""

    def line(self, timeout=5):
        self.sock.settimeout(timeout)
        try:
            while b"\n" not in self.buf:
                chunk = self.sock.recv(65536)
                if not chunk:
                    return None
                self.buf += chunk
        except socket.timeout:
            return None
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def send(self, value):
        self.sock.sendall((json.dumps(value) + "\n").encode())

    def close(self):
        self.sock.close()


def pbpaste():
    return subprocess.run(["pbpaste"], capture_output=True, text=True).stdout


def pbcopy(text):
    subprocess.run(["pbcopy"], input=text, text=True, check=True)


if sys.platform == "darwin":
    tempfile.tempdir = "/tmp"  # Unix socket paths must stay short
tmp = tempfile.mkdtemp(prefix="ar-input-")
try:
    base = Path(tmp)
    host_home, ctl_home, denied_home = base / "host", base / "controller", base / "denied"
    (host_home / "host").mkdir(parents=True)
    (host_home / "host" / "ai-remote.toml").write_text("[host]\napproval_prompt = false\n", encoding="utf-8")
    (denied_home / "host").mkdir(parents=True)
    (denied_home / "host" / "ai-remote.toml").write_text(
        '[host]\napproval_prompt = false\n\n[[rule]]\ntool = "mouse"\naction = "deny"\n', encoding="utf-8")
    env = {k: v for k, v in os.environ.items() if k != "AI_REMOTE_HOST"}

    def cli(*args, home=ctl_home, timeout=120):
        return subprocess.run([str(options.cli), "--home", str(home), *map(str, args)], capture_output=True,
                              text=True, encoding="utf-8", errors="replace", timeout=timeout, env=env)

    logs = [open(base / "host.log", "w"), open(base / "denied.log", "w")]
    hosts = [subprocess.Popen([str(options.cli), "--home", str(h), "host", "--no-pair-code"], stdout=l, stderr=l, env=env)
             for h, l in ((host_home, logs[0]), (denied_home, logs[1]))]
    try:
        names = {}
        for home, name in ((host_home, "live"), (denied_home, "denied")):
            ticket = None
            for _ in range(100):
                r = cli("--json", "pair-code", "--level", "control", home=home)
                if r.returncode == 0:
                    ticket = json.loads(r.stdout)["line"]
                    break
                time.sleep(0.2)
            assert ticket, f"host {name} did not start"
            r = cli("pair", ticket, "--name", name)
            check(f"pair {name} (control)", r.returncode == 0, r.stderr.strip()[-200:])
            names[name] = name
        r = cli("hosts")  # starts the agent
        check("agent up", r.returncode == 0, r.stderr[-200:])
        sock = ctl_home / "controller" / "control.sock"
        check("agent socket exists", sock.exists(), str(sock))

        # Live input is for whoever drives: without taking over the stream is refused.
        a = Agent(sock)
        a.send({"op": "input", "host": "live", "request": {"clipboard": True}})
        reply = a.line(20)
        check("refused until this device takes over", reply and not reply.get("ok") and "take over" in reply.get("error", ""), str(reply)[:160])
        a.close()
        for name in ("live", "denied"):
            r = cli("take-over", "--host", name)
            check(f"take over {name}", r.returncode == 0, r.stderr[-160:])

        # A policy that denies the mouse tool refuses the stream.
        a = Agent(sock)
        a.send({"op": "input", "host": "denied", "request": {"clipboard": True}})
        reply = a.line(20)
        check("policy deny refuses the stream", reply and not reply.get("ok") and "policy" in reply.get("error", ""), str(reply)[:160])
        a.close()

        a = Agent(sock)
        a.send({"op": "input", "host": "live", "request": {"clipboard": True}})
        reply = a.line(20)
        check("stream opens", bool(reply and reply.get("ok")), str(reply)[:200])
        result = (reply or {}).get("result", {})
        check("reply names the os and clipboard", result.get("os") in ("windows", "macos", "other") and result.get("clipboard") is True, str(result))

        saved = pbpaste()
        # Opening the stream syncs nothing: only later changes go either way.
        first = a.line(1.5)
        check("nothing is synced when the stream opens", first is None, str(first)[:120])
        # Our own setting is not echoed back; a change made on the host is reported.
        mine = f"ai-remote live input {time.time():.0f}"
        a.send({"clip": mine})
        time.sleep(0.8)
        check("clip sets the host clipboard", pbpaste() == mine)
        echo = a.line(1.0)
        check("the controller's own text is not echoed back", echo is None or echo.get("clip") != mine, str(echo)[:120])
        theirs = mine + " (changed on the host)"
        pbcopy(theirs)
        got = None
        for _ in range(10):
            got = a.line(1.0)
            if got and got.get("clip") == theirs:
                break
        check("a clipboard change on the host is reported", got is not None and got.get("clip") == theirs, str(got)[:120])

        # Events: a move to the screen's centre, shift down/up, a refused lock chord (ctrl+cmd+q on a
        # Mac, win+l on Windows), then release-all. Nothing but the move and shift reaches the desktop.
        a.send({"m": [700, 450]})
        a.send({"k": "shift", "down": True})
        a.send({"k": "shift", "down": False})
        if sys.platform == "darwin":
            a.send({"k": "ctrl", "down": True})
            a.send({"k": "meta", "down": True})
            a.send({"k": "q", "down": True})
        else:
            a.send({"k": "win", "down": True})
            a.send({"k": "l", "down": True})
        a.send({"release": True})
        a.send({"b": "side", "down": True})
        a.send({"w": [0, 999]})
        errs = []
        for _ in range(6):
            l = a.line(1.0)
            if l is None:
                break
            if "err" in l:
                errs.append(l["err"])
        check("the lock chord is refused", any("refused" in e for e in errs), " | ".join(errs)[:200])
        check("a bad button and too many notches are refused", any("button" in e for e in errs) and any("notches" in e for e in errs), " | ".join(errs)[:200])
        # Handing back ends the stream within about a second, with the reason.
        r = cli("hand-back", "--host", "live")
        check("hand back", r.returncode == 0, r.stderr[-160:])
        end = []
        for _ in range(6):
            l = a.line(1.0)
            if l is None:
                break
            end.append(l)
        check("handing back ends the live stream", any("live input ended" in (l.get("err") or "") for l in end), str(end)[-200:])
        a.close()
        time.sleep(0.5)
        audit = (host_home / "host" / "audit.jsonl").read_text(encoding="utf-8").splitlines()
        starts = [json.loads(l) for l in audit if '"input_start"' in l]
        ends = [json.loads(l) for l in audit if '"input_end"' in l]
        refused = [json.loads(l) for l in audit if '"input_refused"' in l]
        check("a refused open is audited", len(refused) >= 1, str(len(refused)))
        check("audit has input_start and input_end", len(starts) == 1 and len(ends) == 1, f"{len(starts)} / {len(ends)}")
        check("input_end says why it ended", "hand" in (ends[0].get("reason", "") if ends else "") or "take over" in (ends[0].get("reason", "") if ends else ""),
              (ends[0].get("reason") if ends else ""))
        end = ends[0] if ends else {}
        # Presses only: shift, then the modifiers of the refused chord (ctrl+cmd on a Mac, win on Windows).
        check("input_end counts the events", end.get("moves") == 1 and end.get("keys") == (3 if sys.platform == "darwin" else 2)
              and end.get("clips_in") == 1 and end.get("clips_out", 0) >= 1 and end.get("errors", 0) >= 3,
              json.dumps({k: end.get(k) for k in ("moves", "keys", "clips_in", "clips_out", "errors")}))
        pbcopy(saved)
    finally:
        for h in hosts:
            h.terminate()
        for h in hosts:
            try:
                h.wait(10)
            except subprocess.TimeoutExpired:
                h.kill()
        for l in logs:
            l.close()
        cli("stop-agent")
finally:
    shutil.rmtree(tmp, ignore_errors=True)

print(f"\n{len(failures)} failure(s)" if failures else "\nall checks passed")
sys.exit(1 if failures else 0)
