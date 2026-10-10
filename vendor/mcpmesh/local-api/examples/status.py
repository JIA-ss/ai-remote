#!/usr/bin/env python3
"""Talk to the mcpmesh daemon from Python — standard library only, no SDK.

`mcpmesh-local/1` is newline-delimited JSON over a same-user local endpoint, so any
language that can open the endpoint and parse JSON can drive the mesh. This example
proves it: resolve the endpoint, complete the handshake, ask for `status`.

Run it with a daemon up (any `mcpmesh` verb starts one):  python3 status.py

The full protocol — every method, the identity contract, the security model — is
documented in docs/local-protocol.md.
"""
import json
import os
import socket
import sys
import tempfile

API_NAME = "mcpmesh-local/1"


def endpoint_path() -> str:
    """The endpoint rule from docs/local-protocol.md ("Finding the local endpoint"):
    $XDG_RUNTIME_DIR when set, non-empty, and absolute; else $TMPDIR; else the platform
    temp dir — then the `mcpmesh/mcpmesh.sock` suffix. (On Windows the endpoint is a
    named pipe instead — \\\\.\\pipe\\mcpmesh-<domain>-<user> — see the spec.)
    """
    xdg = os.environ.get("XDG_RUNTIME_DIR")
    base = xdg if xdg and os.path.isabs(xdg) else (os.environ.get("TMPDIR") or tempfile.gettempdir())
    return os.path.join(base, "mcpmesh", "mcpmesh.sock")


def main() -> None:
    path = endpoint_path()
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        sock.connect(path)
    except OSError:
        sys.exit(f"no daemon reachable at {path} — start one with `mcpmesh status`")
    wire = sock.makefile("rw", encoding="utf-8", newline="\n")

    # The server speaks first: one Hello frame. Check the api name before sending
    # anything — a different name means this is some sibling daemon's endpoint.
    hello = json.loads(wire.readline())
    if hello.get("api") != API_NAME:
        sys.exit(f"unexpected api {hello.get('api')!r} (want {API_NAME!r}) — hanging up")

    # One request frame out, one response frame in. `id`/`jsonrpc` are optional on
    # requests; parameterless methods need no `params` at all.
    wire.write(json.dumps({"method": "status"}) + "\n")
    wire.flush()
    reply = json.loads(wire.readline())
    if "error" in reply:
        sys.exit(f"daemon refused: {reply['error']}")
    status = reply["result"]

    print(f"mcpmesh {status['stack_version']}")
    for svc in status.get("services", []):
        print(f"  serving {svc['name']} -> {', '.join(svc['allow']) or 'nobody yet'}")
    for peer in status.get("peers", []):
        print(f"  {peer['name']} shares: {', '.join(peer['services'])}")
    for probe in status.get("reachability", []):
        if probe.get("age_secs") is None:
            state = "checking…"  # never probed yet — not the same thing as offline
        elif probe["reachable"]:
            rtt = probe.get("rtt_ms")
            state = f"online · {rtt} ms" if rtt is not None else "online"
        else:
            state = "offline"
        print(f"  {probe['name']} is {state}")


if __name__ == "__main__":
    main()
