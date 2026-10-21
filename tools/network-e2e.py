#!/usr/bin/env python3
"""End-to-end check of the network settings on one machine: an isolated host and controller, both set
up with `ai-remote network` (what the desktop app's Settings > Network writes), over real QUIC.

  python tools/network-e2e.py [--cli target/debug/ai-remote]

Checks: the quiet defaults; Local network only on both ends; no pair code there without local
discovery (the ticket instead); pairing and a tool call with no relay;
doctor's `lan` check; a LAN-mode host with no TCP connection at all (no relay, no address lookup); a
host that comes back on another port being unreachable at the stored address and reached again once
it is set with `ai-remote hosts address`; and quiet = true keeping port mapping off. Nothing outside
this machine is contacted in LAN mode; the host listens on a free port, not 47800.
"""
import argparse
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXE = ".exe" if os.name == "nt" else ""
# The CLI prints UTF-8; Windows would otherwise decode with the console code page.
TEXT = {"text": True, "encoding": "utf-8", "errors": "replace"}
parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--cli", type=Path, default=ROOT / f"target/debug/ai-remote{EXE}")
options = parser.parse_args()
failures = []


def check(name, ok, detail=""):
    print(f"{'ok  ' if ok else 'FAIL'}  {name}" + (f"  ({detail})" if detail else ""), flush=True)
    if not ok:
        failures.append(name)
    return ok


def run(*args, home, timeout=90):
    p = subprocess.run([str(options.cli), *args, "--home", str(home)], capture_output=True, timeout=timeout, **TEXT)
    return p.returncode, p.stdout, p.stderr


def run_json(*args, home, timeout=90):
    code, out, err = run(*args, "--json", home=home, timeout=timeout)
    if code != 0:
        raise RuntimeError(f"{' '.join(args)} failed ({code}): {err.strip() or out.strip()}")
    return json.loads(out)


def free_udp_port():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind(("0.0.0.0", 0))
        return s.getsockname()[1]


def set_port(home, port):
    """Puts `port = N` under [network] in the host's ai-remote.toml (written by `ai-remote network`)."""
    path = home / "host" / "ai-remote.toml"
    text = path.read_text()
    text = re.sub(r"(?m)^port = \d+\n", "", text)
    text = text.replace("[network]\n", f"[network]\nport = {port}\n", 1)
    path.write_text(text)


def start_host(home):
    proc = subprocess.Popen([str(options.cli), "host", "--home", str(home)], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, **TEXT)
    deadline = time.monotonic() + 60
    for line in proc.stdout:
        if "is running" in line:
            return proc
        if time.monotonic() > deadline:
            break
    proc.kill()
    raise RuntimeError("the host did not start")


def stop_host(home, proc):
    run("stop-host", home=home)
    try:
        proc.wait(timeout=20)
    except subprocess.TimeoutExpired:
        proc.kill()


def tcp_connections(pid):
    """The host process's TCP connections (any state), as text lines."""
    if os.name == "nt":
        ps = f"Get-NetTCPConnection -OwningProcess {pid} -ErrorAction SilentlyContinue | ForEach-Object {{ $_.RemoteAddress + ':' + $_.RemotePort + ' ' + $_.State }}"
        out = subprocess.run(["powershell", "-NoProfile", "-Command", ps], capture_output=True, **TEXT).stdout
    else:
        out = subprocess.run(["lsof", "-nP", "-a", "-p", str(pid), "-iTCP"], capture_output=True, **TEXT).stdout
        out = "\n".join(out.splitlines()[1:])
    return [line for line in out.splitlines() if line.strip()]


def main():
    if not options.cli.is_file():
        sys.exit(f"{options.cli} not found; build it first (cargo build -p ai-remote)")
    # Short on purpose: the agents' Unix sockets live under it, and their paths have a length limit.
    work = Path(tempfile.mkdtemp(prefix="arne-", dir=None if os.name == "nt" else "/tmp"))
    host_home, ctl_home = work / "host", work / "ctl"
    host = None
    try:
        defaults = run_json("network", home=host_home)
        check("quiet by default", (defaults["mode"], defaults["lan_discovery"], defaults["port_mapping"]) == ("auto", False, False), defaults)

        for home in (host_home, ctl_home):
            changed = run_json("network", "mode", "lan", home=home)
            check(f"Local network only set ({home.name})", changed["mode"] == "lan", changed)
        first_port = free_udp_port()
        set_port(host_home, first_port)

        host = start_host(host_home)
        offer = run_json("pair-code", "--level", "control", "--name", "e2e-ctl", home=host_home)
        check("no pair code without local discovery, the ticket instead", "code" not in offer and offer.get("code_reason") == "lan_discovery_off", {k: offer.get(k) for k in ("code_reason", "code_error")})
        ticket = offer["line"]
        paired = run_json("pair", ticket, "--name", "e2e-host", home=ctl_home)
        check("paired over the local network", paired.get("peer") == "e2e-host", paired)

        info = run_json("info", "--host", "e2e-host", home=ctl_home)
        check("tool call with no relay", "os" in info, list(info)[:5])
        hosts = run_json("hosts", home=ctl_home)
        h = next(x for x in hosts if x["name"] == "e2e-host")
        check("route is direct", h["path"] == "direct", h)
        check("stored address carries the host's port", any(a.endswith(f":{first_port}") for a in h["addresses"]), h["addresses"])

        # Exits 1 when any check fails (a host started without a desktop session fails the screen
        # checks); only the network checks matter here.
        _, out, _ = run("doctor", "--host", "e2e-host", "--json", home=ctl_home)
        doctor = json.loads(out)
        lan = next((c for c in doctor["checks"] if c["id"] == "lan"), None)
        check("doctor reports LAN mode and the host's addresses", lan is not None and lan["grade"] == "ok" and f":{first_port}" in lan["summary"], lan)
        net = next((c for c in doctor["checks"] if c["id"] == "network"), None)
        check("doctor: nothing extra on the network", net is not None and net["summary"].startswith("quiet"), net)

        conns = tcp_connections(host.pid)
        check("LAN-mode host has no TCP connection (no relay, no address lookup)", not conns, conns)

        # The host comes back on another port; the controller's agent restarts too (changing a
        # network setting restarts it), so neither side remembers the other's new address.
        stop_host(host_home, host)
        host = None
        second_port = free_udp_port()
        set_port(host_home, second_port)
        host = start_host(host_home)
        run_json("network", "mode", "lan", home=ctl_home)
        started = time.monotonic()
        code, _, err = run("info", "--host", "e2e-host", home=ctl_home)
        unreachable = "unreachable" in err or "timed out" in err
        check("old address is a dead end (dial times out)", code != 0 and unreachable, f"{time.monotonic() - started:.0f} s, {err.strip()[:160]}")

        local = next(a for a in h["addresses"] if not a.startswith("["))
        new_addr = f"{local.rsplit(':', 1)[0]}:{second_port}"
        set_addr = run_json("hosts", "address", "e2e-host", new_addr, home=ctl_home)
        check("address set by hand", set_addr["addresses"] == [new_addr], set_addr)
        started = time.monotonic()
        info = run_json("info", "--host", "e2e-host", home=ctl_home)
        check("reached again at the new address", "os" in info, f"{time.monotonic() - started:.2f} s")

        path = ctl_home / "controller" / "ai-remote.toml"
        path.write_text(path.read_text().replace("[network]\n", "[network]\nquiet = true\n", 1))
        code, _, err = run("network", "upnp", "on", home=ctl_home)
        check("quiet = true refuses to turn port mapping on", code != 0 and "quiet = true" in err, err.strip()[:160])
        shown = run_json("network", home=ctl_home)
        check("and reports the lock", shown["locked"] and not shown["port_mapping"], shown)
    finally:
        if host:
            stop_host(host_home, host)
        # Changing a network setting stops the controller's agent; nothing starts it again.
        run("network", "mode", "auto", home=ctl_home)
        shutil.rmtree(work, ignore_errors=True)
    print(f"\n{'all checks passed' if not failures else 'FAILED: ' + ', '.join(failures)}")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
