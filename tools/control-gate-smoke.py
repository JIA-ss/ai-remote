#!/usr/bin/env python3
"""Real MCP/QUIC regression for driver and request cancellation, without desktop input.

Only isolated profiles, temporary shell markers and an ephemeral approval key are
used. No default permissions, client config, user files or launch agents change.
--probe records old behaviour without asserting the fix.
"""
import argparse
import base64
import concurrent.futures
import json
import select
import shlex
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--cli", type=Path, default=ROOT / "target/debug/ai-remote")
parser.add_argument("--out", type=Path, default=ROOT / "target/control-gate-validation")
parser.add_argument("--probe", action="store_true")
options = parser.parse_args()
CLI, OUT = options.cli.resolve(), options.out.resolve()
OUT.mkdir(parents=True, exist_ok=True)
report = {}


def run(*args, timeout=30):
    result = subprocess.run([str(a) for a in args], capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"command failed ({result.returncode}): {result.stderr[-1000:]}")
    return result.stdout


def wait_for(check, description, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.05)
    raise AssertionError(f"timed out: {description}")


with tempfile.TemporaryDirectory(prefix="ar-gate-", dir="/tmp") as tmp:
    home = Path(tmp)
    host_home, controller_home = home / "host", home / "controller"
    (host_home / "host").mkdir(parents=True)
    (host_home / "host/ai-remote.toml").write_text(
        '[host]\napproval_prompt = false\n[[rule]]\ntool = "shell"\naction = "allow"\n')
    log = open(OUT / "host.log", "w")
    host = subprocess.Popen([str(CLI), "--home", str(host_home), "host", "--no-pair-code"],
                            stdout=log, stderr=log)
    bridge = None
    pool = concurrent.futures.ThreadPoolExecutor(max_workers=1)

    def command(profile, *args):
        return json.loads(run(CLI, "--home", profile, "--json", *args))

    def call(tool, args):
        return command(controller_home, "call", tool, json.dumps(args), "--host", "gate-e2e")

    def send(value):
        bridge.stdin.write(json.dumps(value) + "\n")
        bridge.stdin.flush()

    def reply(request_id):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            assert select.select([bridge.stdout], [], [], max(0, deadline - time.monotonic()))[0], "MCP timeout"
            value = json.loads(bridge.stdout.readline())
            if value.get("id") == request_id:
                assert "error" not in value, value
                return value["result"]
        raise AssertionError("MCP response missing")

    def mcp_call(request_id, tool, args):
        send({"jsonrpc": "2.0", "id": request_id, "method": "tools/call", "params": {
            "name": tool, "arguments": {**args, "host": "gate-e2e"}}})

    def pending():
        return command(controller_home, "approvals", "--host", "gate-e2e")["pending"]

    def withdraw_if_old(waiting):
        # Old builds leave approval waiting after cancellation; deny it so a
        # baseline run remains bounded and never approves/starts the command.
        if waiting:
            command(controller_home, "deny", waiting[0]["id"], "--host", "gate-e2e")

    def slow_script(prefix):
        started, finished = home / f"{prefix}-started", home / f"{prefix}-finished"
        return started, finished, {"shell": "sh", "command":
            f"touch {shlex.quote(str(started))}; sleep 5; touch {shlex.quote(str(finished))}"}

    try:
        wait_for(lambda: (host_home / "host/control.sock").exists(), "test host ready")
        ticket = command(host_home, "pair-code", "--level", "admin")["line"]
        command(controller_home, "pair", ticket, "--name", "gate-e2e")
        report["host_session"] = call("system_info", {})["structuredContent"]["session"]

        # Running calls must stop even if control is handed straight back.
        started, takeover_marker, args = slow_script("takeover")
        active = pool.submit(call, "shell", args)
        wait_for(started.exists, "running shell started")
        stamp = time.monotonic()
        command(controller_home, "take-over", "--host", "gate-e2e", "--let-watch")
        command(controller_home, "hand-back", "--host", "gate-e2e")
        result = active.result(timeout=15)
        report["takeover"] = {"cancelled": result.get("isError", False),
                              "response_secs": round(time.monotonic() - stamp, 3),
                              "stale_marker_arrived": takeover_marker.exists()}

        command(controller_home, "take-over", "--host", "gate-e2e", "--let-watch")
        assert not call("system_info", {}).get("isError"), "owner allowed observation"
        assert call("shell", {"command": "printf blocked", "shell": "sh"}).get("isError"), "agent must wait"
        person = command(controller_home, "call", "shell", json.dumps({"command": "printf person", "shell": "sh"}),
                         "--host", "gate-e2e", "--person")
        assert not person.get("isError"), "person session must still run"
        command(controller_home, "hand-back", "--host", "gate-e2e")
        command(controller_home, "pause", "--host", "gate-e2e")
        assert call("system_info", {}).get("isError"), "paused agent cannot observe"
        command(controller_home, "resume", "--host", "gate-e2e")
        assert not call("shell", {"command": "printf resumed", "shell": "sh"}).get("isError")
        report["watch_person_and_resume"] = "passed"

        bridge = subprocess.Popen([str(CLI), "--home", str(controller_home), "mcp"],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, bufsize=1)
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "control-gate-smoke", "version": "1"}}})
        reply(1)
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        started, cancel_marker, args = slow_script("cancel")
        mcp_call(2, "shell", args)
        wait_for(started.exists, "MCP shell started")
        send({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 2}})
        mcp_call(3, "shell", {"command": "printf fresh", "shell": "sh"})
        fresh = reply(3)
        assert not fresh.get("isError"), fresh
        report["request_cancel"] = {"fresh_call_passed": True}

        # A public disposable key enables remote-only approval waiting. This is
        # installed only into this test host; no approval is ever signed.
        private, public = home / "key.pem", home / "key.der"
        run("openssl", "genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out", private)
        run("openssl", "pkey", "-in", private, "-pubout", "-outform", "DER", "-out", public)
        peer = command(host_home, "peers")[0]["name"]
        key = "ecdsa-p256-sha256:" + base64.b64encode(public.read_bytes()).decode()
        command(host_home, "approvers", "add", peer, key)
        approval_marker = home / "approval-should-never-run"
        args = {"command": f"touch {shlex.quote(str(approval_marker))}", "shell": "sh"}
        mcp_call(4, "jobs_start", args)
        wait_for(pending, "remote approval waiting")
        command(controller_home, "take-over", "--host", "gate-e2e")
        command(controller_home, "hand-back", "--host", "gate-e2e")
        time.sleep(.3)
        waiting = pending()
        report["approval_takeover"] = {"withdrawn": not waiting}
        withdraw_if_old(waiting)
        assert reply(4).get("isError"), "approval never authorizes an action here"

        mcp_call(5, "jobs_start", args)
        wait_for(pending, "second approval waiting")
        send({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 5}})
        time.sleep(.3)
        waiting = pending()
        report["approval_request_cancel"] = {"withdrawn": not waiting}
        withdraw_if_old(waiting)

        # Enough time for an erroneously detached five-second shell to finish.
        time.sleep(5)
        report["request_cancel"]["stale_marker_arrived"] = cancel_marker.exists()
        assert not approval_marker.exists(), "unapproved job ran"
        if not options.probe:
            assert report["takeover"]["cancelled"] and not takeover_marker.exists(), report
            assert not cancel_marker.exists(), report
            assert report["approval_takeover"]["withdrawn"], report
            assert report["approval_request_cancel"]["withdrawn"], report
        report["result"] = "probe" if options.probe else "passed"
    finally:
        report.setdefault("result", "failed")
        if bridge:
            bridge.stdin.close()
            try:
                bridge.wait(timeout=5)
            except subprocess.TimeoutExpired:
                bridge.terminate()
                bridge.wait(timeout=5)
        for profile, stop in [(host_home, "stop-host"), (controller_home, "stop-agent")]:
            try:
                run(CLI, "--home", profile, stop, timeout=10)
            except Exception:
                pass
        pool.shutdown(wait=True)
        if host.poll() is None:
            host.terminate()
        host.wait(timeout=10)
        log.close()
        audit = host_home / "host/audit.jsonl"
        if audit.exists():
            (OUT / "audit.jsonl").write_bytes(audit.read_bytes())
        (OUT / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))

print(json.dumps({"result": report["result"], "evidence": str(OUT)}, ensure_ascii=False))
