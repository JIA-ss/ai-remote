#!/usr/bin/env python3
"""File previews in the desktop app against a real coding agent on another computer.

On the other computer, start a test host with its own profile whose host/ and controller/ ai-remote.toml
have `[network]` with `quiet = true` and `mode = "lan"` (Local network only: no relay, no address lookup),
and a free `port` for the host (not 47800, which an installed host holds), and make a pairing ticket for
level admin:

  ai-remote --home <test home> host --no-pair-code
  ai-remote --home <test home> --json pair-code --level admin      # the "line" field

Then here (macOS; build first: cargo build -p ai-remote && (cd apps/desktop && npx tauri build --debug --no-bundle)):

  python3 tools/desktop-chat-preview-remote.py --ticket '<line>' --work 'C:\\Users\\me\\preview-work' \
      [--agent 'Claude Code'] [--big out/big.png] [--out /tmp/chat-preview-remote]

The app runs on a throwaway profile (quiet and LAN-only too), paired as `e2e-<pid>`; tools/desktop-chat-preview-real.js
drives it. This spends the agent's quota on the other computer (one short task).
"""
import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
p.add_argument("--ticket", required=True)
p.add_argument("--work", required=True, help="the agent's folder on the other computer (it must exist)")
p.add_argument("--agent", default="Claude Code")
p.add_argument("--big", help="a large previewable file already in the folder, also timed (relative path)")
p.add_argument("--cli", type=Path, default=ROOT / "target/debug/ai-remote")
p.add_argument("--app", type=Path, default=ROOT / "target/debug/ai-remote-desktop")
p.add_argument("--out", type=Path, default=Path("/tmp/chat-preview-remote"))
p.add_argument("--secs", type=int, default=600)
o = p.parse_args()
o.out.mkdir(parents=True, exist_ok=True)
env = {k: v for k, v in os.environ.items() if k not in ("AI_REMOTE_HOST", "AI_REMOTE_HOME")}

with tempfile.TemporaryDirectory(prefix="adr-", dir="/tmp") as tmp:
    desk = Path(tmp) / "desk"
    for role in ("host", "controller"):
        (desk / role).mkdir(parents=True)
        (desk / role / "ai-remote.toml").write_text('[network]\nquiet = true\nmode = "lan"\n', encoding="utf-8")
    cli = lambda *a: subprocess.run([str(o.cli), "--home", str(desk), *a], capture_output=True, text=True, env=env, timeout=120)
    name = f"e2e-{os.getpid()}"
    r = cli("pair", o.ticket, "--name", name)
    if r.returncode != 0:
        sys.exit(f"pairing failed: {r.stderr or r.stdout}")
    host = json.loads(cli("--json", "hosts").stdout)[0]["name"]
    print(f"paired with {host!r}; route: {cli('hosts').stdout.strip()}")
    script = Path(tmp) / "drive.js"
    script.write_text((ROOT / "tools/desktop-chat-preview-real.js").read_text(encoding="utf-8").replace(
        "__CONFIG__", json.dumps({"host": host, "work": o.work, "agent": o.agent, "big": o.big})), encoding="utf-8")
    app_env = {**env, "AI_REMOTE_HOME": str(desk), "AI_REMOTE_SMOKE": str(o.out / "zz-final.png"),
               "AI_REMOTE_SMOKE_SECS": str(o.secs), "AI_REMOTE_SMOKE_SCRIPT": str(script),
               "AI_REMOTE_SMOKE_QUERY": f"view=session&host={host}&panel=chat"}
    app = subprocess.Popen([str(o.app)], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, env=app_env)
    passed = None
    for line in app.stderr:
        line = line.rstrip()
        if line.startswith("[page e2e] "):
            text = line[len("[page e2e] "):]
            print(text, flush=True)
            if text == "all passed" or re.fullmatch(r"\d+ failed", text):
                passed = text == "all passed"
        elif line.startswith("[page ") and ("error" in line or "warn" in line):
            print("  app:", line[:300])
        elif line.startswith("[smoke]"):
            print("  " + line)
    app.wait(timeout=60)
    cli("unpair", host)
    cli("stop-agent")
print(f"screenshots: {o.out}")
sys.exit(0 if passed else 1)
