#!/usr/bin/env python3
"""The iPhone app's chat code against a real host, on a Mac: an isolated host with the stand-in agent from
tools/chat-smoke.py, a session started by another controller (the command line), and the phone's Swift
chat code (PhoneClient, PhoneChatStore, PhoneChatLog) built for macOS with `tools/ios.sh mac-core`
around the same embedded controller the app uses. It lists the agents, starts a session from the new-chat
form (and sees a bad folder refused), lists sessions, opens the command line's one (read-write: any admin
device may talk), then sends, queues, stops and resumes on its own.

  python3 tools/phone-chat-smoke.py [--cli target/debug/ai-remote]

Every home (host, command line, phone) gets `[network] quiet = true` and `mode = "lan"` in host/ and
controller/, and the run stops unless the host reports quiet mode: no mDNS or UPnP on the office network,
and no relay or address lookup (nothing outside this machine). The host listens on a free UDP port, not
47800 (an installed host may hold it).

macOS only; needs Command Line Tools, not Xcode. The UIKit parts of the view are not exercised here.
"""
import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--cli", type=Path, default=ROOT / "target/debug/ai-remote")
options = parser.parse_args()

source = (ROOT / "tools/chat-smoke.py").read_text(encoding="utf-8")
FAKE_AGENT = source.split("FAKE_AGENT = r'''", 1)[1].split("'''", 1)[0]

subprocess.run([str(ROOT / "tools/ios.sh"), "mac-core"], check=True, cwd=ROOT)
# Release, as on the phone: a debug core's futures overflow the stack of Swift's cooperative threads.
subprocess.run(["cargo", "build", "-p", "ar-mobile", "--release"], check=True, cwd=ROOT)
generated = ROOT / "apps/ios/Generated"
out = ROOT / "target/phone-chat-smoke"
out.mkdir(parents=True, exist_ok=True)
subprocess.run(["swiftc", "-parse-as-library", str(ROOT / "tools/PhoneChatSmoke.swift"), "-target", "arm64-apple-macos14.0",
                "-I", str(generated), "-L", str(generated), "-lAIRemotePhone", "-lAIRemoteCore",
                "-Xlinker", "-rpath", "-Xlinker", str(generated), "-o", str(out / "phone-chat-smoke")], check=True)

with tempfile.TemporaryDirectory(prefix="apc-", dir="/tmp") as tmp:
    base = Path(tmp)
    host_home, ctl_home, phone, work = base / "host", base / "controller", base / "phone", base / "work"
    work.mkdir()
    # Files for the @ menu, the folder picker and the previews; one outside the folder that the agent
    # names (it may be previewed) and one it never names (it may not).
    for rel, body in {"docs/report.html": "<h1>report</h1>", "docs/notes.md": "# notes\n", "Cargo.toml": "[package]\n",
                      "README.md": "# readme\n"}.items():
        (work / rel).parent.mkdir(parents=True, exist_ok=True)
        (work / rel).write_text(body, encoding="utf-8")
    (work / "out").mkdir()
    (work / ".hidden").mkdir()
    subprocess.run(["sips", "-s", "format", "png", "-z", "40", "64", "/System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/GenericDocumentIcon.icns",
                    "--out", str(work / "out/chart.png")], check=True, capture_output=True)
    (base / "outside").mkdir()
    (base / "outside/named.md").write_text("# named\n", encoding="utf-8")
    (base / "outside/secret.md").write_text("# secret\n", encoding="utf-8")
    (host_home / "host").mkdir(parents=True)
    # Quiet and Local network only on every role of every home, the phone's too.
    quiet = '[network]\nquiet = true\nmode = "lan"\n'
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        probe.bind(("0.0.0.0", 0))
        host_port = probe.getsockname()[1]
    (host_home / "host" / "ai-remote.toml").write_text(
        '[host]\napproval_prompt = false\n\n' + quiet + f'port = {host_port}\n' + '\n[[rule]]\ntool = "chat_*"\naction = "allow"\n',
        encoding="utf-8")
    for home in (host_home, ctl_home, phone):
        for role in ("host", "controller"):
            (home / role).mkdir(parents=True, exist_ok=True)
            if not (home / role / "ai-remote.toml").exists():
                (home / role / "ai-remote.toml").write_text(quiet, encoding="utf-8")
    fake = base / "fake_agent.py"
    fake.write_text(FAKE_AGENT, encoding="utf-8")
    (host_home / "host" / "chat.toml").write_text(
        f'[[agent]]\nid = "fake"\nbase = "claude"\nname = "Fake agent"\ncommand = {json.dumps(sys.executable)}\n'
        f'args = ["-u", {json.dumps(str(fake))}]\n', encoding="utf-8")
    env = {k: v for k, v in os.environ.items() if k != "AI_REMOTE_HOST"}

    def cli(*args, home=ctl_home):
        return subprocess.run([str(options.cli), "--home", str(home), *map(str, args)], capture_output=True, text=True,
                              timeout=120, env=env)

    def pair_code():
        for _ in range(100):
            r = cli("--json", "pair-code", "--level", "admin", home=host_home)
            if r.returncode == 0:
                return json.loads(r.stdout)["line"]
            time.sleep(0.2)
        raise SystemExit("host did not start")

    log = open(base / "host.log", "w")
    host = subprocess.Popen([str(options.cli), "--home", str(host_home), "host", "--no-pair-code"], stdout=log, stderr=log, env=env)
    try:
        doctor = cli("doctor", home=host_home).stdout
        assert "network           quiet" in doctor or "quiet mode" in doctor, "the test host is not in quiet mode:\n" + doctor
        assert cli("pair", pair_code(), "--name", "self").returncode == 0, "the command line did not pair"
        r = cli("chat", "run", "fake", "from the desktop", "--cwd", work, "--keep")
        assert r.returncode == 0, r.stderr
        foreign = next(s["id"] for s in json.loads(cli("--json", "chat", "sessions").stdout)["sessions"] if s["state"] != "ended")
        result = subprocess.run([str(out / "phone-chat-smoke"), str(phone), pair_code(), str(work), foreign], timeout=180,
                                env={**env, "DYLD_LIBRARY_PATH": str(ROOT / "target/release"), "HOSTNAME": "smoke-iphone"})
        code = result.returncode
    finally:
        cli("stop-host", home=host_home)
        try:
            host.wait(timeout=10)
        except subprocess.TimeoutExpired:
            host.kill()
        cli("stop-agent")
        log.close()
    if code != 0:
        print((base / "host.log").read_text()[-3000:])
    sys.exit(code)
