#!/usr/bin/env python3
"""Test the same Swift/UniFFI phone core against a real Mac host; this is not iPhone acceptance."""
import json
from pathlib import Path
import subprocess
import tempfile
import time

root = Path(__file__).resolve().parents[1]
generated = root / "apps/ios/Generated"
out = root / "target/phone-validation"
out.mkdir(parents=True, exist_ok=True)
cli = root / "target/debug/ai-remote"
subprocess.run(["swiftc", "-parse-as-library", "-target", "arm64-apple-macos14.0", str(root / "tools/PhoneCoreSmoke.swift"),
                "-I", str(generated), "-L", str(generated), "-lAIRemoteCore", "-lAIRemotePhone", "-Xlinker", "-rpath", "-Xlinker", str(generated), "-o", str(out / "PhoneCoreSmoke")], check=True)
with tempfile.TemporaryDirectory(prefix="ar-phone-", dir="/tmp") as tmp:
    tmp = Path(tmp)
    log = open(out / "host.log", "w")
    host = subprocess.Popen([str(cli), "--home", str(tmp / "host"), "host", "--no-pair-code"], stdout=log, stderr=log)
    try:
        for _ in range(150):
            if (tmp / "host/host/control.sock").exists(): break
            if host.poll() is not None: raise RuntimeError("test host stopped")
            time.sleep(.1)
        result = subprocess.run([str(cli), "--home", str(tmp / "host"), "--json", "pair-code", "--level", "control"], capture_output=True, text=True, timeout=30, check=True)
        ticket = tmp / "ticket"
        ticket.write_text(json.loads(result.stdout)["line"])
        ticket.chmod(0o600)
        phone = subprocess.run([str(out / "PhoneCoreSmoke"), str(tmp / "phone"), str(ticket), str(out / "report.json")], capture_output=True, text=True, timeout=90)
        (out / "swift.log").write_text(phone.stdout + phone.stderr)
        if phone.returncode: raise RuntimeError(f"Swift phone-core test failed; see {out / 'swift.log'}")
        print(phone.stdout)
    finally:
        subprocess.run([str(cli), "--home", str(tmp / "host"), "stop-host"], capture_output=True, timeout=15)
        if host.poll() is None: host.terminate()
        host.wait(timeout=10); log.close()
