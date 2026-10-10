#!/usr/bin/env python3
"""Native macOS regression: takeover and MCP cancellation stop active/queued input.

Uses isolated profiles and a disposable Cocoa text field, never user documents.
Build with tools/macos.sh build --debug --bundles app first. --probe records the
old behaviour without asserting the fix; --out keeps before/after evidence separate.
"""
import argparse
import base64
import concurrent.futures
import json
import select
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--probe", action="store_true")
parser.add_argument("--out", type=Path, default=ROOT / "target/input-control-validation")
options = parser.parse_args()
CLI = ROOT / "target/debug/bundle/macos/ai-remote.app/Contents/MacOS/ai-remote"
OUT = options.out.resolve()
OUT.mkdir(parents=True, exist_ok=True)
report = {}


class DesktopUnavailable(Exception):
    """A preflight blocker, distinct from a failed input regression."""


def run(*args, timeout=45):
    result = subprocess.run([str(a) for a in args], capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"command failed ({result.returncode}): {result.stderr[-1000:]}")
    return result.stdout


def wait_for(check, description, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.05)
    raise AssertionError(f"timed out: {description}")


with tempfile.TemporaryDirectory(prefix="ar-input-", dir="/tmp") as tmp:
    home = Path(tmp)
    host_home, controller_home = home / "host", home / "controller"
    state_file = home / "input.json"
    fixture = bridge = None
    log = open(OUT / "host.log", "w")
    host = subprocess.Popen([str(CLI), "--home", str(host_home), "host", "--no-pair-code"],
                            stdout=log, stderr=log)
    pool = concurrent.futures.ThreadPoolExecutor(max_workers=2)

    def command(profile, *args):
        return json.loads(run(CLI, "--home", profile, "--json", *args))

    def call(tool, args, person=False):
        extra = ["--person"] if person else []
        return command(controller_home, "call", tool, json.dumps(args), "--host", "input-e2e", *extra)

    def successful(value):
        assert not value.get("isError"), value.get("content")
        return value.get("structuredContent") or value

    def state():
        value = json.loads(state_file.read_text())
        assert value["frontmost"], "fixture lost focus; input regression stopped"
        return value

    def clear():
        state()
        successful(call("keyboard", {"keys": "cmd+a"}, person=True))
        successful(call("keyboard", {"keys": "backspace"}, person=True))
        wait_for(lambda: state()["text"] == "", "empty disposable field")

    def seen_intent(intent):
        path = host_home / "host/audit.jsonl"
        if not path.exists():
            return False
        return any(v.get("kind") == "decision" and v.get("intent") == intent
                   for v in map(json.loads, path.read_text().splitlines()))

    def send(value):
        bridge.stdin.write(json.dumps(value) + "\n")
        bridge.stdin.flush()

    def reply(request_id, timeout=40):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            assert select.select([bridge.stdout], [], [], max(0, deadline - time.monotonic()))[0], "MCP timeout"
            value = json.loads(bridge.stdout.readline())
            if value.get("id") == request_id:
                assert "error" not in value, value
                return value["result"]
        raise AssertionError("MCP response missing")

    # A ten-second paced sequence, so cancellation can be observed during execution.
    long_keys = {"keys": " ".join(["x"] * 10), "repeat": 100}
    try:
        wait_for(lambda: (host_home / "host/control.sock").exists(), "test host ready")
        ticket = command(host_home, "pair-code", "--level", "control")["line"]
        command(controller_home, "pair", ticket, "--name", "input-e2e")
        info = successful(call("system_info", {}))
        if not info["session"]["desktop_usable"]:
            report.update(result="blocked", reason=info["session"].get("problem", "desktop unavailable"))
            raise DesktopUnavailable(report["reason"])
        run("swiftc", ROOT / "tools/macos-input-fixture.swift", "-o", home / "fixture")
        fixture = subprocess.Popen([str(home / "fixture"), str(state_file)],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        wait_for(lambda: state_file.exists(), "fixture window ready")
        # A background launch on recent macOS may refuse NSApp.activate. Target
        # only our verified PID; never select or activate an unrelated window.
        if not json.loads(state_file.read_text())["frontmost"]:
            run("osascript", "-e", f'tell application "System Events" to set frontmost of '
                f'(first application process whose unix id is {fixture.pid}) to true')
        wait_for(lambda: state_file.exists() and json.loads(state_file.read_text())["frontmost"], "fixture focused")
        successful(call("mouse", {"action": "click", **state()["field"], "space": "screen"}, person=True))
        wait_for(lambda: state()["editing"], "fixture field editing")

        # The queued call has passed the gate before takeover. Immediate handback
        # must not revive either call, even if a worker hasn't seen the human state.
        active = pool.submit(call, "keyboard", {**long_keys, "intent": "takeover-active"})
        wait_for(lambda: len(state()["text"]) >= 3, "active input reached target")
        queued = pool.submit(call, "keyboard", {"text": "QUEUED", "intent": "takeover-queued"})
        wait_for(lambda: seen_intent("takeover-queued"), "second input passed gate and queued")
        if not options.probe:
            overflow = call("keyboard", {"text": "OVERFLOW"})
            assert overflow.get("isError"), "third input call must be rejected immediately"
            assert "input busy" in json.dumps(overflow), overflow
            report["bounded_queue"] = "third call refused"
        command(controller_home, "take-over", "--host", "input-e2e", "--let-watch")
        # Observation remains allowed while input is stopped.
        successful(call("screenshot", {}))
        report["watch_during_takeover"] = "passed"
        command(controller_home, "hand-back", "--host", "input-e2e")
        active_result, queued_result = active.result(timeout=40), queued.result(timeout=40)
        time.sleep(.3)
        text = state()["text"]
        report["takeover"] = {"active_error": active_result.get("isError", False),
                              "queued_error": queued_result.get("isError", False),
                              "typed": len(text), "queued_marker_arrived": "QUEUED" in text}
        if not options.probe:
            assert active_result.get("isError") and queued_result.get("isError"), report["takeover"]
            assert len(text) < 1000 and "QUEUED" not in text, report["takeover"]
        clear()

        # Real stdio MCP bridge -> QUIC -> host notifications/cancelled. The next
        # request must run without waiting for the cancelled blocking worker.
        bridge = subprocess.Popen([str(CLI), "--home", str(controller_home), "mcp"],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, bufsize=1)
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "input-control-smoke", "version": "1"}}})
        reply(1)
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        send({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "keyboard", "arguments": {**long_keys, "host": "input-e2e", "intent": "cancel-active"}}})
        wait_for(lambda: len(state()["text"]) >= 3, "MCP input reached target")
        cancelled_at = time.monotonic()
        send({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {
            "requestId": 2, "reason": "native input regression"}})
        send({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "keyboard", "arguments": {"text": "FRESH", "host": "input-e2e"}}})
        successful(reply(3))
        wait_for(lambda: "FRESH" in state()["text"], "fresh request reached target")
        time.sleep(.3)
        text = state()["text"]
        elapsed = time.monotonic() - cancelled_at
        report["request_cancel"] = {"typed_x": text.count("x"), "fresh_marker_arrived": "FRESH" in text,
                                    "fresh_request_secs": round(elapsed, 3)}
        if not options.probe:
            assert text.count("x") < 1000 and elapsed < 4, report["request_cancel"]
        clear()

        command(controller_home, "pause", "--host", "input-e2e")
        refused = call("keyboard", {"text": "AGENT"})
        assert refused.get("isError"), refused
        successful(call("keyboard", {"text": "PERSON"}, person=True))
        wait_for(lambda: state()["text"] == "PERSON", "human input while paused")
        command(controller_home, "resume", "--host", "input-e2e")
        successful(call("keyboard", {"text": "FRESH"}))
        wait_for(lambda: state()["text"] == "PERSONFRESH", "fresh input after resume")
        report["human_and_resume"] = "passed"
        shot = call("screenshot", {})
        successful(shot)
        pixels = next(c for c in shot["content"] if c["type"] == "image")
        (OUT / "fixture.jpg").write_bytes(base64.b64decode(pixels["data"]))
        report["result"] = "probe" if options.probe else "passed"
    except DesktopUnavailable:
        pass
    finally:
        report.setdefault("result", "failed")
        if state_file.exists():
            report["fixture_final"] = json.loads(state_file.read_text())
        if bridge:
            bridge.stdin.close()
            try:
                bridge.wait(timeout=5)
            except subprocess.TimeoutExpired:
                bridge.terminate()
                bridge.wait(timeout=5)
        # Only processes owned by this isolated test are stopped.
        for profile, stop in [(host_home, "stop-host"), (controller_home, "stop-agent")]:
            try:
                run(CLI, "--home", profile, stop, timeout=10)
            except Exception:
                pass
        pool.shutdown(wait=True)
        if fixture:
            fixture.terminate()
            fixture.wait(timeout=5)
        if host.poll() is None:
            host.terminate()
        host.wait(timeout=10)
        log.close()
        audit = host_home / "host/audit.jsonl"
        if audit.exists():
            (OUT / "audit.jsonl").write_bytes(audit.read_bytes())
        (OUT / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))

print(json.dumps({"result": report["result"], "reason": report.get("reason"), "evidence": str(OUT)}, ensure_ascii=False))
if report["result"] == "blocked":
    raise SystemExit(2)
