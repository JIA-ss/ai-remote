#!/usr/bin/env python3
"""End-to-end check of the remote-control command line on one machine: an isolated host and controller
pair over real QUIC (admin level; the host allows shell, jobs and file tools without asking except
files_mkdir, which policy denies so a refusal is covered too), then every command once, with its exit
code and output checked.

  python tools/cli-remote-smoke.py [--cli target/debug/ai-remote.exe] [--big-mib 256]

Screenshots and window lists need an unlocked desktop; they are reported as skipped otherwise.
Prints one line per check and a throughput figure for push/pull of --big-mib MiB (loopback, so it
measures ai-remote's own overhead, not a network).
"""
import argparse
import hashlib
import json
import os
import queue
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXE = ".exe" if os.name == "nt" else ""
parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--cli", type=Path, default=ROOT / f"target/debug/ai-remote{EXE}")
parser.add_argument("--big-mib", type=int, default=256)
options = parser.parse_args()

failures = []


def check(name, ok, detail=""):
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + (f"  ({detail})" if detail else ""))
    if not ok:
        failures.append(name)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


with tempfile.TemporaryDirectory(prefix="ar-cli-", ignore_cleanup_errors=True) as tmp:
    base = Path(tmp)
    host_home, ctl_home = base / "host", base / "controller"
    remote = base / "remote"          # what the host's file tools see
    local = base / "local"            # the controller's side
    local.mkdir()
    (host_home / "host").mkdir(parents=True)
    (host_home / "host" / "ai-remote.toml").write_text(
        '[host]\napproval_prompt = false\n\n'
        '[[rule]]\ntool = "files_mkdir"\naction = "deny"\n\n'
        '[[rule]]\ntool = "files_*"\naction = "allow"\n\n'
        '[[rule]]\ntool = "shell"\naction = "allow"\n\n'
        '[[rule]]\ntool = "jobs_*"\naction = "allow"\n\n'
        '[[rule]]\ntool = "process_*"\naction = "allow"\n', encoding="utf-8")
    env = {k: v for k, v in os.environ.items() if k != "AI_REMOTE_HOST"}

    def cli(*args, home=ctl_home, timeout=120, stdin=None, extra_env=None):
        e = {**env, **(extra_env or {})}
        return subprocess.run([str(options.cli), "--home", str(home), *map(str, args)], capture_output=True,
                              text=True, encoding="utf-8", errors="replace", timeout=timeout, input=stdin, env=e)

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

        r = cli("info")
        check("info", r.returncode == 0 and "level admin" in r.stdout, r.stdout.strip().splitlines()[-1:] or r.stderr[-200:])

        if os.name == "nt":
            r = cli("exec", "Write-Output hello; [Console]::Error.WriteLine('oops'); exit 3")
            check("exec passes stdout, stderr and the exit code", r.returncode == 3 and r.stdout.strip() == "hello"
                  and "oops" in r.stderr, f"code {r.returncode}")
            r = cli("exec", "Start-Sleep 5", "--timeout", "1")
            check("exec timeout exits 124", r.returncode == 124, f"code {r.returncode}")
            (local / "in.txt").write_text("from stdin\n", encoding="utf-8")
            r = cli("exec", "[Console]::In.ReadToEnd()", "--stdin", local / "in.txt")
            check("exec --stdin", r.returncode == 0 and "from stdin" in r.stdout, r.stdout[-200:])
            r = cli("--json", "exec", "$env:SMOKE_X", "--env", "SMOKE_X=42")
            v = json.loads(r.stdout) if r.returncode == 0 else {}
            check("exec --json --env", v.get("stdout", "").strip() == "42", r.stdout[-200:])
            job_cmd = "1..3 | % { Write-Output line$_; Start-Sleep -Milliseconds 300 }; exit 4"
        else:
            r = cli("exec", "echo hello; echo oops >&2; exit 3")
            check("exec passes stdout, stderr and the exit code", r.returncode == 3 and r.stdout.strip() == "hello")
            job_cmd = "for i in 1 2 3; do echo line$i; sleep 0.3; done; exit 4"

        r = cli("exec", "exit 0", extra_env={"AI_REMOTE_HOST": "no-such-host"})
        check("AI_REMOTE_HOST selects the host (unknown one: did not run, 255)", r.returncode == 255, r.stderr[-160:])

        r = cli("job", "start", job_cmd)
        job = r.stdout.strip()
        check("job start prints the id", r.returncode == 0 and len(job) > 0, r.stderr[-200:])
        r = cli("job", "log", job, "-f", timeout=60)
        check("job log -f follows to the end", r.returncode == 0 and all(f"line{i}" in r.stdout for i in (1, 2, 3)),
              r.stdout.strip().replace("\n", " ")[-120:])
        r = cli("job", "wait", job)
        check("job wait exits with the job's code", r.returncode == 4, f"code {r.returncode}")
        r = cli("job", "list")
        check("job list", r.returncode == 0 and job in r.stdout)
        r = cli("job", "status", job)
        check("job status", r.returncode == 0 and "exited (4)" in r.stdout, r.stdout.splitlines()[:1])
        long_job = cli("job", "start", "Start-Sleep 30" if os.name == "nt" else "sleep 30").stdout.strip()
        r = cli("job", "wait", long_job, "--timeout", "1")
        check("job wait --timeout exits 124 while it runs", r.returncode == 124, f"code {r.returncode}")
        r = cli("job", "kill", long_job)
        check("job kill", r.returncode == 0 and "killed" in r.stdout, r.stdout.strip())

        shot = local / "shot.png"
        r = cli("shot", "-o", shot)
        if r.returncode == 2 and "desktop" in r.stderr:
            print("skip  shot (desktop not usable)")
        else:
            # PNG over the result budget arrives as JPEG; the file name must then say .jpg.
            jpg = shot.with_suffix(".jpg")
            ok = r.returncode == 0 and (shot.exists() and shot.read_bytes()[:4] == b"\x89PNG"
                                        or not shot.exists() and jpg.read_bytes()[:3] == b"\xff\xd8\xff")
            check("shot -o x.png saves the image under the right extension", ok, r.stdout.strip() or r.stderr[-200:])
        r = cli("windows")
        check("windows", r.returncode == 0, r.stderr[-200:])

        r = cli("--json", "ps", "--pid", str(host.pid))
        ps = json.loads(r.stdout) if r.returncode == 0 else {}
        check("ps --pid finds the host", ps.get("count") == 1 and ps["processes"][0]["pid"] == host.pid, r.stderr[-200:])
        r = cli("ps", "ai-remote")
        check("ps <name> lists it", r.returncode == 0 and str(host.pid) in r.stdout, r.stderr[-200:])
        r = cli("kill", "--pid", str(host.pid))
        check("kill refuses the host itself", r.returncode == 2 and "host itself" in r.stderr, r.stderr.strip()[-160:])
        sleeper = subprocess.Popen(["cmd", "/C", "ping -n 30 127.0.0.1 > NUL"] if os.name == "nt" else ["sh", "-c", "sleep 30"],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(0.5)
        r = cli("--json", "kill", "--pid", str(sleeper.pid), "--intent", "smoke")
        killed = json.loads(r.stdout) if r.returncode == 0 else {}
        check("kill --pid stops a process tree", r.returncode == 0 and killed.get("count", 0) >= 1 and killed.get("still_running") == [],
              (r.stdout + r.stderr).strip()[-200:])
        try:
            sleeper.wait(timeout=5)
            check("the killed process is gone", True)
        except subprocess.TimeoutExpired:
            sleeper.kill()
            check("the killed process is gone", False)

        r = cli("mkdir", remote / "denied")
        check("a refused call exits 2 with the host's reason", r.returncode == 2 and "policy" in r.stderr, r.stderr.strip())

        small = local / "small.txt"
        small.write_text("hello from the controller\n" * 1000, encoding="utf-8")
        r = cli("push", small, f"{remote}{os.sep}sub{os.sep}", "--mkdir")
        landed = remote / "sub" / "small.txt"
        check("push into a new folder (--mkdir, trailing separator)", r.returncode == 0 and landed.exists()
              and sha256(landed) == sha256(small), (r.stdout or r.stderr).strip()[-200:])
        r = cli("push", small, landed)
        check("push refuses to replace without --force (exit 2)", r.returncode == 2 and "--force" in r.stderr,
              r.stderr.strip()[-160:])
        small.write_text("changed\n", encoding="utf-8")
        r = cli("push", small, landed, "--force")
        check("push --force replaces", r.returncode == 0 and landed.read_text(encoding="utf-8") == "changed\n")
        other = local / "other.txt"
        other.write_text("another\n", encoding="utf-8")
        r = cli("push", other, remote / "sub")
        check("push to an existing folder keeps the name", r.returncode == 0 and (remote / "sub" / "other.txt").exists(),
              (r.stdout or r.stderr).strip()[-160:])
        r = cli("push", small, "relative/path.txt")
        check("relative remote paths are refused", r.returncode == 2 and "absolute" in r.stderr, r.stderr.strip()[-160:])

        big = local / "big.bin"
        with open(big, "wb") as f:
            for _ in range(options.big_mib):
                f.write(os.urandom(1 << 20))
        t = time.monotonic()
        r = cli("--json", "push", big, remote / "big.bin", timeout=600)
        push_s = time.monotonic() - t
        v = json.loads(r.stdout) if r.returncode == 0 else {}
        check(f"push {options.big_mib} MiB", r.returncode == 0 and v.get("sha256") == sha256(big),
              f"{options.big_mib / push_s:.0f} MiB/s wall clock incl. hashing" if r.returncode == 0 else r.stderr[-200:])
        back = local / "back"
        back.mkdir()
        t = time.monotonic()
        r = cli("--json", "pull", remote / "big.bin", back, timeout=600)
        pull_s = time.monotonic() - t
        check(f"pull {options.big_mib} MiB into a folder", r.returncode == 0 and sha256(back / "big.bin") == sha256(big),
              f"{options.big_mib / pull_s:.0f} MiB/s wall clock" if r.returncode == 0 else r.stderr[-200:])
        r = cli("pull", remote / "big.bin", back)
        check("pull refuses to replace a local file without --force", r.returncode == 1 and "exists" in r.stderr)
        r = cli("pull", remote / "nope.bin")
        check("pull of a missing file exits 2", r.returncode == 2, r.stderr.strip()[-160:])
        check("no partial files left behind", not list(remote.rglob("*.part")) and not list(local.rglob("*.part")))

        # A takeover while an upload runs: the host stops reading, and its reason must still reach the CLI.
        race = local / "race.bin"
        with open(race, "wb") as f:
            for _ in range(192):
                f.write(os.urandom(1 << 20))
        proc = subprocess.Popen([str(options.cli), "--home", str(ctl_home), "push", str(race), str(remote / "race.bin")],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8",
                                errors="replace", env=env)
        deadline = time.monotonic() + 60
        while not list(remote.glob(".race.bin.*.part")) and proc.poll() is None and time.monotonic() < deadline:
            time.sleep(0.05)
        took = cli("take-over")
        out, err = proc.communicate(timeout=120)
        cli("hand-back")
        check("a takeover stops an upload midway and the CLI gets the host's reason",
              took.returncode == 0 and proc.returncode == 2 and "control changed" in err
              and not (remote / "race.bin").exists() and not list(remote.glob("*.part")),
              f"code {proc.returncode}: {(err or out).strip()[-160:]}")

        r = cli("ls", remote, "--pattern", "*.bin")
        check("ls --pattern", r.returncode == 0 and "big.bin" in r.stdout and "sub" not in r.stdout, r.stdout.strip())
        r = cli("stat", remote / "big.bin")
        check("stat", r.returncode == 0 and f"{options.big_mib << 20} bytes" in r.stdout, r.stdout.strip())
        r = cli("rm", remote / "sub")
        check("rm of a full folder needs -r", r.returncode == 2 and "pass -r" in r.stderr, r.stderr.strip()[-120:])
        r = cli("rm", remote / "sub", "-r")
        r2 = cli("stat", remote / "sub")
        check("rm -r", r.returncode == 0 and "does not exist" in r2.stdout, r.stderr[-160:])
        r = cli("rm", host_home)
        check("the host's own profile cannot be deleted", r.returncode == 2 and "ai-remote" in r.stderr)

        r = cli("hosts", "default", "self")
        r2 = cli("hosts")
        check("hosts default", r.returncode == 0 and "(default)" in r2.stdout, r2.stdout.strip())
        r = cli("hosts", "default", "--clear")
        check("hosts default --clear", r.returncode == 0 and "(default)" not in cli("hosts").stdout)

        r = cli("doctor", "--host", "self")
        check("doctor --host asks the host", r.returncode in (0, 1) and "Checked by the host" in r.stdout,
              (r.stdout.strip().splitlines() or [""])[-1])

        # An MCP client sees the file tools it can finish, not the two whose ticket only the CLI redeems.
        bridge = subprocess.Popen([str(options.cli), "--home", str(ctl_home), "mcp"], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, encoding="utf-8",
                                  env=env)
        replies = queue.Queue()
        threading.Thread(target=lambda: [replies.put(json.loads(l)) for l in bridge.stdout], daemon=True).start()

        def rpc(value):
            bridge.stdin.write(json.dumps(value) + "\n")
            bridge.stdin.flush()
            return replies.get(timeout=30) if "id" in value else None

        try:
            rpc({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "cli-smoke", "version": "1"}}})
            rpc({"jsonrpc": "2.0", "method": "notifications/initialized"})
            names = {t["name"] for t in rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})["result"]["tools"]}
            check("MCP lists files_list/stat/mkdir/delete, hides files_push/pull",
                  {"files_list", "files_stat", "files_mkdir", "files_delete"} <= names
                  and not names & {"files_push", "files_pull"}, f"{len(names)} tools")
            r = rpc({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": {"name": "files_pull", "arguments": {"path": str(remote / "big.bin")}}})["result"]
            check("MCP files_pull points to the CLI", r.get("isError") is True and "ai-remote pull" in r["content"][0]["text"])
        finally:
            bridge.stdin.close()
            bridge.wait(timeout=10)

        audit = [json.loads(l) for l in (host_home / "host" / "audit.jsonl").read_text(encoding="utf-8").splitlines()]
        transfers = [a for a in audit if a.get("kind") == "transfer"]
        check("transfers are in the audit log", any(a["outcome"] == "ok" and a["tool"] == "files_push" for a in transfers)
              and any(a["tool"] == "files_pull" for a in transfers), f"{len(transfers)} transfer records")
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
