# Implementation review: remote control and input cancellation

Reviewed on 2026-10-11 against pinned source snapshots. This review informed a
bounded change to ai-remote's existing gate and input executor; no upstream code
or dependency was imported.

## Sources and decisions

| Source | Actual implementation reviewed | Decision for ai-remote |
| --- | --- | --- |
| [NexusAgentX/rustdesk, mcp-v0.1.9](https://github.com/NexusAgentX/rustdesk/tree/a0df1515f706d97a06f59ae7cf0c26410f7e7e76), AGPL-3.0 | [`control.rs`](https://github.com/NexusAgentX/rustdesk/blob/a0df1515f706d97a06f59ae7cf0c26410f7e7e76/src/automation/control.rs): permits carry generation/connection epoch; `Permit::check` refuses old control. [`input.rs`](https://github.com/NexusAgentX/rustdesk/blob/a0df1515f706d97a06f59ae7cf0c26410f7e7e76/src/automation/input.rs): cancellable queue and pacing, capacity of one executing/one queued batch. [`wire.rs`](https://github.com/NexusAgentX/rustdesk/blob/a0df1515f706d97a06f59ae7cf0c26410f7e7e76/src/automation/wire.rs): checks authority again at send and releases held inputs during transition. | Snapshot a revocable grant at admission, invalidate old grants permanently, check blocking input before every event, bound the queue and wait asynchronously. Use existing MCP-session cancellation for connection lifetime rather than adding a second network epoch system. |
| [zavora-ai/computer-use-mcp](https://github.com/zavora-ai/computer-use-mcp/tree/97c01db95b4a6a948ec76489f5b7e2e253683e89), MIT | [`user-activity.ts`](https://github.com/zavora-ai/computer-use-mcp/blob/97c01db95b4a6a948ec76489f5b7e2e253683e89/src/session/user-activity.ts): physical-user activity guard, background PID delivery exemption, capability disclosure when monitoring is unavailable. [`lock.ts`](https://github.com/zavora-ai/computer-use-mcp/blob/97c01db95b4a6a948ec76489f5b7e2e253683e89/src/session/lock.ts): cross-process writer lease; a live but slow holder is not reclaimed by age alone. | Retain explicit owner takeover as the current control contract. Physical activity detection and background native input need separate platform work and validation. Preserve the existing exclusive IPC daemon ownership; do not replace a live host based on a timeout. |

## Change in ai-remote

Previously, the gate checked the current driver only at entry. The blocking input
worker checked session cancellation but did not see request cancellation or
takeover. Pointer moves inside a drag had no cancellation check. A call queued
behind the input mutex could therefore survive takeover, including an immediate
handback, and cancelled requests could keep typing.

`Turns::admits` now atomically returns a grant token together with admission.
Takeover/pause cancels control grants. Observation can continue when the owner
explicitly allows watching. A cancelled token is never reused, so old work cannot
revive after handback/resume. Person sessions retain the existing semantics.

The gate applies session/request/grant cancellation during approval and tool
execution. Its request child token is also cancelled when the call future ends,
so a detached blocking input worker sees cancellation. Input checks all three
tokens before every operation and before constructing its native backend. The
input queue permits one executing batch and one waiter; its guards remain with
the actual worker if the async caller drops the JoinHandle.

Cleanup retains failed releases for a second release attempt. Cancelling an
approval withdraws its pending record, but native blocking dialogs can remain
visible; their input-blocking guard now lives with the actual dialog until it is
dismissed.

The result can report cancellation after some native events were already sent.
This does not promise to undo OS events, external side effects, or background jobs
that already started. Clients should observe the host before replacing an action.

## Reproduce verification

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python3 tools/control-gate-smoke.py
tools/macos.sh build --debug --bundles app
python3 tools/input-control-smoke.py
```

The gate smoke uses isolated profiles and temporary shell markers, actual stdio
MCP, QUIC, driver commands and a disposable approval key. It can run while the
desktop is locked. Native input validation needs an unlocked macOS desktop with
Screen Recording and Accessibility already granted; it refuses before input when
the desktop is unavailable. It uses only its own Cocoa text field and saves the
report, audit and screenshot under `target/input-control-validation/`.

Native validation covers active plus queued input across takeover/handback,
continued observation when allowed, queue overflow, real MCP request cancellation,
human input during pause and fresh agent input after resume. Unit tests exercise
cancelled drag movement, cleanup after failed release and permanently invalidated
grants. Windows and physical iPhone runtime validation are separate requirements.

The attempted Jev `code-search` returned Rust excerpts as `REVIEW` because its
symbol extraction did not support this source language; candidate scope was also
incomplete. No semantic inference request was made (zero recorded model tokens).
Exact searches and manual source review supplied the decisions above. No Jev
integration or runtime model dependency was added.

## Validation evidence on this Mac

The old packaged CLI and the updated CLI ran the same core gate probe with
separate temporary profiles (`target/control-gate-before/report.json` and
`target/control-gate-validation/report.json`). The disposable shell writes a
start marker, sleeps five seconds, and then writes a finish marker. Taking over
and immediately handing back left the old call running: it answered after
4.972 seconds and wrote the stale finish marker. The updated call answered with
cancellation in 0.032 seconds and never wrote that marker. These are local gate
probe timings, not a GUI-input or network-latency benchmark.

Both waiting-approval probes remained pending on the old CLI and were withdrawn
on the updated CLI: one through takeover/handback, the other through real MCP
`notifications/cancelled`. Shell request cancellation already worked before this
change and continued to pass. The updated probe also confirms allowed observation,
person sessions and fresh calls after resume. Workspace tests and Clippy passed.

Native mouse/keyboard acceptance is currently blocked by the locked macOS
desktop. Screen Recording and Accessibility preflight passed. The native script
is ready to run after unlock; recorder tests passing does not establish delivery
to the actual macOS window. Windows and physical iPhone E2E were not run here.
