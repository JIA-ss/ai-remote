#!/usr/bin/env python3
"""Where a live-view frame's time goes on the controller, between processes: joins the hop trace of
the controller agent and the desktop app (both written with AI_REMOTE_TRACE_HOPS=<dir>) with the
page's lines in the app's log ("Live video: hops [...]", written when localStorage
"ai-remote.hops" is "1"; the debug app's smoke mode sets it with the variable).

  python3 tools/hops-report.py <dir> <app log>

Stages, all on this machine's wall clock: the agent has read the frame from the host (QUIC) ->
written it to the app's socket -> the app's native side has read it -> handed it to the page ->
the page has it -> decoded -> drawn. Prints per-hop percentiles over the frames seen at every stage.
"""
import json
import sys
from pathlib import Path

d, log = Path(sys.argv[1]), Path(sys.argv[2])
frames = {}


def put(ts, stage, us):
    try:
        frames.setdefault(int(ts), {}).setdefault(stage, int(us))
    except ValueError:
        pass  # a line cut off when the process stopped


for line in (d / "agent.tsv").read_text().splitlines():
    if line.count("\t") != 2:
        continue
    ts, a, b = line.split("\t")
    put(ts, "agent_in", a)
    put(ts, "agent_out", b)
for line in (d / "native.tsv").read_text().splitlines():
    if line.count("\t") != 2:
        continue
    ts, a, b = line.split("\t")
    put(ts, "native_in", a)
    put(ts, "native_out", b)
for line in log.read_text(errors="replace").splitlines():
    if "Live video: hops " in line:
        try:
            batch = json.loads(line.split("Live video: hops ", 1)[1])
        except ValueError:
            continue  # cut off
        for stage, ts, us in batch:
            put(ts, stage, us)

order = ["agent_in", "agent_out", "native_in", "native_out", "page", "decoded", "drawn"]
full = [f for f in frames.values() if all(s in f for s in order)]
print(f"frames: {len(frames)} seen, {len(full)} at every stage")


def pct(values, p):
    values = sorted(values)
    return values[min(len(values) - 1, int(p * len(values)))] if values else float("nan")


print(f"{'hop':32} {'p50 ms':>8} {'p90 ms':>8} {'p99 ms':>8}")
for a, b in zip(order, order[1:]):
    v = [(f[b] - f[a]) / 1000 for f in full]
    print(f"{a + ' -> ' + b:32} {pct(v, .5):8.2f} {pct(v, .9):8.2f} {pct(v, .99):8.2f}")
v = [(f["page"] - f["agent_in"]) / 1000 for f in full]
print(f"{'agent_in -> page (processes)':32} {pct(v, .5):8.2f} {pct(v, .9):8.2f} {pct(v, .99):8.2f}")
v = [(f["drawn"] - f["agent_in"]) / 1000 for f in full]
print(f"{'agent_in -> drawn (all)':32} {pct(v, .5):8.2f} {pct(v, .9):8.2f} {pct(v, .99):8.2f}")
