#!/usr/bin/env python3
"""Native macOS E2E with isolated profiles and a disposable Cocoa input window.

Build first: tools/macos.sh build --debug --bundles app
Requires swiftc, ffmpeg and ffprobe. Screenshots stay in target/macos-validation.
"""
import base64
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "target/debug/bundle/macos/ai-remote.app/Contents/MacOS"
CLI = APP / "ai-remote"
OUT = ROOT / "target/macos-validation"
OUT.mkdir(parents=True, exist_ok=True)
report = {}


def run(*args, timeout=45, env=None):
    p = subprocess.run([str(a) for a in args], capture_output=True, text=True, timeout=timeout, env=env)
    if p.returncode:
        # Pairing tickets and full desktop contents must never enter error reports.
        raise RuntimeError(f"command failed (exit {p.returncode}): {p.stderr[-1500:]}")
    return p.stdout


with tempfile.TemporaryDirectory(prefix="ar-mac-", dir="/tmp") as tmp:
    profile = Path(tmp)
    host_home, controller_home = profile / "host", profile / "controller"
    fixture = None
    host_log = open(OUT / "host.log", "w")
    host = subprocess.Popen([str(CLI), "--home", str(host_home), "host", "--no-pair-code"],
                            stdout=host_log, stderr=host_log)

    def command(home, *args):
        return json.loads(run(CLI, "--home", home, "--json", *args))

    def call(name, args=None):
        v = command(controller_home, "call", name, json.dumps(args or {}), "--host", "mac-e2e")
        if v.get("isError"):
            raise RuntimeError(f"{name} failed: {v.get('content')}")
        return v.get("structuredContent") or v

    try:
        # Simultaneous first requests reproduced Unix socket replacement in the desktop.
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            results = list(pool.map(lambda _: command(controller_home, "hosts"), range(8)))
        assert all(v == [] for v in results), "all cold clients must reach the new controller"
        status = command(controller_home, "status")
        assert status["agent"]["device"], "controller identity must be available"
        report["concurrent_cold_start"] = "8 clients, one identity, all status requests succeeded"
        for _ in range(100):
            if (host_home / "host/control.sock").exists():
                break
            if host.poll() is not None:
                raise RuntimeError("test host exited")
            time.sleep(.1)
        ticket = command(host_home, "pair-code", "--level", "control")["line"]
        pair = command(controller_home, "pair", ticket, "--name", "mac-e2e")
        assert pair["level"] == "control"
        report["pairing"] = "control granted"
        info = call("system_info")
        assert info["os"]["family"] == "macos" and info["session"]["desktop_usable"]
        report["system"] = {"os": info["os"], "monitors": info["monitors"]}
        doctor = call("doctor")
        assert doctor["overall"] != "fail", doctor
        report["doctor"] = doctor
        shot = command(controller_home, "call", "screenshot", "{}", "--host", "mac-e2e")
        assert not shot.get("isError"), "screenshot failed"
        pixels = next(c for c in shot["content"] if c["type"] == "image")
        (OUT / "screenshot.jpg").write_bytes(base64.b64decode(pixels["data"]))
        report["screenshot"] = "decoded MCP image saved"

        if "--skip-input" not in sys.argv:
            run("swiftc", ROOT / "tools/macos-input-fixture.swift", "-o", profile / "fixture")
            state = profile / "input.json"
            fixture = subprocess.Popen([str(profile / "fixture"), str(state)], stdout=subprocess.DEVNULL,
                                       stderr=subprocess.DEVNULL)
            for _ in range(50):
                if state.exists() and json.loads(state.read_text())["frontmost"]: break
                time.sleep(.1)
            windows = call("list_windows")["windows"]
            b = next(w["bounds"] for w in windows if w["title"] == "ai-remote macOS E2E" and w["pid"] == fixture.pid)
            assert b["width"] > 0
            def input_call(tool, args):
                assert json.loads(state.read_text())["frontmost"], "test window lost focus; input test stopped"
                return call(tool, args)

            for name in ["button", "field"]:
                point = json.loads(state.read_text())[name]
                input_call("mouse", {"action": "click", **point, "space": "screen"})
                if name == "button":
                    for _ in range(60):
                        if json.loads(state.read_text())["clicks"] == 1: break
                        time.sleep(.1)
                    else:
                        actual = json.loads(state.read_text())
                        # macOS can consume the initial click to activate a window. Retry only
                        # after readback proves this disposable button has not fired.
                        assert actual["frontmost"] and actual["clicks"] == 0, actual
                        input_call("mouse", {"action": "click", **actual["button"], "space": "screen"})
                        for _ in range(60):
                            if json.loads(state.read_text())["clicks"] == 1: break
                            time.sleep(.1)
                        else: raise AssertionError(f"native button did not receive the click: {json.loads(state.read_text())}")
                else:
                    for _ in range(60):
                        if json.loads(state.read_text())["editing"]: break
                        time.sleep(.1)
                    else: raise AssertionError("native text field did not receive focus")
            input_call("keyboard", {"text": "First text 超过二十字符的中文与英文输入验证"})
            # Wait for the actual target to consume asynchronous OS events.
            for _ in range(60):
                if json.loads(state.read_text())["text"] == "First text 超过二十字符的中文与英文输入验证": break
                time.sleep(.1)
            else: raise AssertionError(f"literal text did not arrive intact: {json.loads(state.read_text())['text']!r}")
            input_call("keyboard", {"keys": "cmd+a"})
            input_call("keyboard", {"text": "中文与Command键通过"})
            for _ in range(60):
                actual = json.loads(state.read_text())
                if actual["clicks"] == 1 and actual["text"] == "中文与Command键通过": break
                time.sleep(.1)
            else: raise AssertionError(f"native input readback mismatch: {actual}")
            report["native_input"] = {"clicks": actual["clicks"], "text": actual["text"]}
        else:
            report["native_input"] = "skipped by --skip-input"

        raw = OUT / "live.h264"
        report["live_stream"] = run(CLI, "--home", controller_home, "view", "--host", "mac-e2e",
                                    "--seconds", "5", "--max-fps", "10", "--out", raw)
        report["video"] = json.loads(run("ffprobe", "-v", "error", "-select_streams", "v:0",
                                         "-show_entries", "stream=codec_name,width,height", "-of", "json", raw))
        assert report["video"]["streams"][0]["codec_name"] == "h264"
        run("ffmpeg", "-v", "error", "-y", "-i", raw, "-frames:v", "1", OUT / "decoded.png")

        # Launch the packaged app with no checkout CLI or development server dependency.
        env = {k: v for k, v in os.environ.items() if not k.startswith("AI_REMOTE_")}
        env.update(PATH="/usr/bin:/bin:/usr/sbin:/sbin", AI_REMOTE_HOME=str(controller_home),
                   AI_REMOTE_SMOKE=str(OUT / "native-session.png"), AI_REMOTE_SMOKE_SECS="15",
                   AI_REMOTE_SMOKE_QUERY="view=session&host=mac-e2e&shotlang=zh")
        p = subprocess.run([str(APP / "ai-remote-desktop")], capture_output=True, text=True,
                           env=env, timeout=45)
        (OUT / "desktop.log").write_text(p.stderr)
        assert p.returncode == 0 and "[smoke] saved" in p.stderr, p.stderr
        assert "window visible: true" in p.stderr, "normal macOS startup left the window hidden"
        assert "[page error]" not in p.stderr, p.stderr
        diag = next(line.removeprefix("[page smoke] ") for line in p.stderr.splitlines() if line.startswith("[page smoke] "))
        diag = json.loads(diag)
        assert any(w >= 320 and h >= 180 for w, h in diag["canvases"]), "WKWebView never decoded a live frame"
        report["packaged_app"] = {"result": "real live frame decoded in WKWebView", "diagnostics": diag}
        report["autostart_plan"] = run(CLI, "--home", host_home, "host", "install", "--dry-run")
        report["result"] = "passed"
    finally:
        if "result" not in report:
            report["result"] = "failed"
        if fixture:
            fixture.terminate()
            fixture.wait(timeout=5)
        for home, stop in [(host_home, "stop-host"), (controller_home, "stop-agent")]:
            try: run(CLI, "--home", home, stop, timeout=10)
            except Exception: pass
        if host.poll() is None:
            host.terminate()
        host.wait(timeout=10)
        host_log.close()
        (OUT / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))

print(json.dumps({"result": report["result"], "evidence": str(OUT)}, ensure_ascii=False))
