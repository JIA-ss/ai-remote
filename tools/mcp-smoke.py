#!/usr/bin/env python3
"""Initialize the real stdio MCP bridge without altering any client's configuration."""
import json
from pathlib import Path
import select
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
cli = root / "target/debug/bundle/macos/ai-remote.app/Contents/MacOS/ai-remote"
with tempfile.TemporaryDirectory(prefix="ar-mcp-", dir="/tmp") as home:
    bridge = subprocess.Popen([str(cli), "--home", home, "mcp"], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
    def send(value):
        bridge.stdin.write(json.dumps(value) + "\n")
        bridge.stdin.flush()
    def reply():
        assert select.select([bridge.stdout], [], [], 25)[0], "MCP response timed out"
        value = json.loads(bridge.stdout.readline())
        assert "error" not in value, value
        return value
    try:
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "ai-remote-install-smoke", "version": "1"}}})
        init = reply()
        assert init["id"] == 1 and init["result"]["serverInfo"]["name"]
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        send({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        tools = reply()
        assert tools["id"] == 2 and isinstance(tools["result"]["tools"], list)
        status = subprocess.run([str(cli), "--home", home, "--json", "agents"], capture_output=True,
                                text=True, check=True, timeout=15)
        agents = json.loads(status.stdout)
        assert any(a["client"] == "ai-remote-install-smoke" for a in agents), agents
        print(json.dumps({"initialize": "passed", "tools/list": "passed", "connected_agent": "observed",
                          "tool_count": len(tools["result"]["tools"])}, ensure_ascii=False))
    finally:
        bridge.stdin.close()
        try: bridge.wait(timeout=5)
        except subprocess.TimeoutExpired:
            bridge.terminate()
            bridge.wait(timeout=5)
        subprocess.run([str(cli), "--home", home, "stop-agent"], capture_output=True, timeout=15)
