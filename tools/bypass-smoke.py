#!/usr/bin/env python3
"""End-to-end check of bypass on one machine: an isolated host and controller pair over real QUIC,
the controller paired at observe level, the host with no approval prompt and a deny rule. Before
bypass the controller's admin calls are refused; with `ai-remote bypass on` they run without approval
and past the deny rule; turning it off, letting it expire, lowering the level and a takeover all
stop them again.

  python tools/bypass-smoke.py [--cli target/debug/ai-remote.exe]
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
options = parser.parse_args()

failures = []


def check(name, ok, detail=""):
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + (f"  ({detail})" if detail else ""))
    if not ok:
        failures.append(name)


with tempfile.TemporaryDirectory(prefix="ar-bypass-", ignore_cleanup_errors=True) as tmp:
    base = Path(tmp)
    host_home, ctl_home, remote = base / "host", base / "controller", base / "remote"
    remote.mkdir()
    (host_home / "host").mkdir(parents=True)
    (host_home / "host" / "ai-remote.toml").write_text(
        '[host]\napproval_prompt = false\n\n[[rule]]\ntool = "files_delete"\naction = "deny"\n', encoding="utf-8")
    env = {k: v for k, v in os.environ.items() if k != "AI_REMOTE_HOST"}

    def cli(*args, home=ctl_home, timeout=120):
        return subprocess.run([str(options.cli), "--home", str(home), *map(str, args)], capture_output=True,
                              text=True, encoding="utf-8", errors="replace", timeout=timeout, env=env)

    def host_cli(*args):
        return cli(*args, home=host_home)

    def mcp_tools():
        """The tool names an MCP client sees through `ai-remote mcp`."""
        bridge = subprocess.Popen([str(options.cli), "--home", str(ctl_home), "mcp"], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, encoding="utf-8", env=env)
        lines = [json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                     "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "bypass-smoke", "version": "1"}}}),
                 json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                 json.dumps({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})]
        try:
            for line in lines:
                bridge.stdin.write(line + "\n")
                bridge.stdin.flush()
                if '"id": 1' in line:
                    bridge.stdout.readline()
            reply = json.loads(bridge.stdout.readline())
            return {t["name"] for t in reply["result"]["tools"]}
        finally:
            bridge.stdin.close()
            bridge.wait(timeout=10)

    say = "Write-Output hi" if os.name == "nt" else "echo hi"
    log = open(base / "host.log", "w")
    host = subprocess.Popen([str(options.cli), "--home", str(host_home), "host", "--no-pair-code"],
                            stdout=log, stderr=log, env=env)
    try:
        ticket = None
        for _ in range(100):
            r = host_cli("--json", "pair-code", "--level", "observe", "--name", "ctl")
            if r.returncode == 0:
                ticket = json.loads(r.stdout)["line"]
                break
            time.sleep(0.2)
        assert ticket, "host did not start"
        check("pair (observe)", cli("pair", ticket, "--name", "self").returncode == 0)

        r = cli("exec", say)
        check("before bypass: an observe device cannot run shell", r.returncode == 255 and "requires level" in r.stderr,
              r.stderr.strip()[-120:])
        r = host_cli("bypass", "on", "nobody")
        check("bypass on for an unknown device fails", r.returncode != 0)

        check("an agent's tool list has no shell before bypass", "shell" not in mcp_tools())
        r = host_cli("bypass", "on", "ctl")
        check("bypass on (host)", r.returncode == 0 and "bypass on for ctl" in r.stdout, r.stdout.strip()[:120])
        check("an agent's tool list has shell and the file tools with bypass",
              {"shell", "jobs_start", "files_list"} <= mcp_tools())
        r = host_cli("bypass")
        check("bypass status lists it", r.returncode == 0 and "BYPASS" in r.stdout, r.stdout.strip())
        check("peers marks it", "BYPASS" in host_cli("peers").stdout)

        r = cli("exec", say)
        check("with bypass: shell runs without approval", r.returncode == 0 and r.stdout.strip() == "hi",
              (r.stderr or r.stdout).strip()[-120:])
        victim = remote / "victim.txt"
        victim.write_text("x", encoding="utf-8")
        r = cli("rm", victim)
        check("with bypass: a deny rule does not stop it", r.returncode == 0 and not victim.exists(), r.stderr.strip()[-120:])
        src = base / "src.bin"
        src.write_bytes(os.urandom(1 << 20))
        r = cli("push", src, remote / "pushed.bin")
        check("with bypass: file transfers work", r.returncode == 0 and (remote / "pushed.bin").exists(), r.stderr.strip()[-120:])
        r = cli("--json", "info")
        you = json.loads(r.stdout)["you"] if r.returncode == 0 else {}
        check("system_info reports it (real level kept)", you.get("bypass") is True and you.get("level") == "observe", str(you))
        r = cli("access")
        check("access reports it", r.returncode == 0 and "bypass: on" in r.stdout)
        r = host_cli("--json", "doctor")
        checks = {c["id"]: c for c in json.loads(r.stdout)["checks"]} if r.stdout.strip() else {}
        check("doctor warns while it is on", checks.get("bypass", {}).get("grade") == "warn", str(checks.get("bypass")))
        audit = [json.loads(l) for l in (host_home / "host" / "audit.jsonl").read_text(encoding="utf-8").splitlines()]
        decisions = [a for a in audit if a.get("kind") == "decision" and a.get("tool") == "shell"]
        check("audit marks bypassed calls", any(a.get("bypass") is True and a["decision"] == "allow" for a in decisions)
              and any(a.get("kind") == "bypass" and a.get("on") is True for a in audit))

        cli("take-over")
        r = cli("exec", say)
        check("a takeover still stops the agent", r.returncode == 255 and "took over" in r.stderr, r.stderr.strip()[-120:])
        cli("hand-back")

        r = host_cli("bypass", "off", "ctl")
        check("bypass off", r.returncode == 0 and "bypass off for ctl" in r.stdout)
        r = cli("exec", say)
        check("after bypass off: refused again", r.returncode == 255 and "requires level" in r.stderr)

        host_cli("bypass", "on", "--all", "--for", "3s")
        r = cli("exec", say)
        check("bypass on --all --for 3s works at once", r.returncode == 0, r.stderr.strip()[-120:])
        time.sleep(3.5)
        r = cli("exec", say)
        check("and ends by itself", r.returncode == 255 and "requires level" in r.stderr)

        host_cli("bypass", "on", "ctl")
        host_cli("level", "ctl", "none")
        host_cli("level", "ctl", "observe")
        r = host_cli("bypass")
        check("taking the level away ends the bypass", "BYPASS" not in r.stdout, r.stdout.strip())
    finally:
        subprocess.run([str(options.cli), "--home", str(ctl_home), "stop-agent"], capture_output=True, timeout=30)
        host.terminate()
        try:
            host.wait(timeout=10)
        except subprocess.TimeoutExpired:
            host.kill()
        log.close()

print(f"\n{'all checks passed' if not failures else f'{len(failures)} failed: ' + ', '.join(failures)}")
sys.exit(1 if failures else 0)
