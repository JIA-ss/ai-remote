# ai-remote architecture

> Status: draft for milestone M0. Sections marked *(planned)* are not implemented yet.

## 1. What ai-remote is

ai-remote is an open-source remote control system where **AI agents are first-class operators**.
A human can still watch and take over at any time, but the primary interface of a controlled
machine is a set of structured, permissioned, audited tools — not just a video stream.

| Role | Platforms | What it does |
|---|---|---|
| **Host** (controlled machine) | Windows, macOS | Runs `ai-remote host`. Exposes capabilities (screen, UI tree, input, shell, files) as MCP tools over an encrypted P2P connection. Enforces per-peer permissions, asks its owner for approval, writes an audit log. |
| **Controller** | Windows, macOS, iOS *(planned)* | Connects to one or more hosts. Either a human UI, or a bridge that lets any MCP-capable agent (Claude Code, Cursor, custom agents) drive the hosts. |

iOS can only be a controller: iOS does not allow a third-party app to control the device it runs on.

## 2. Design principles

1. **AI is a first-class client.** The host speaks the Model Context Protocol (MCP) natively.
   Structured UI (accessibility tree with element references) is the primary way to act;
   pixel-level mouse/keyboard is the fallback. Screenshots are scaled for model consumption.
2. **Humans stay in charge.** Every peer has a permission level, destructive tools can require
   approval on the host, every call is audited, and the host owner can revoke a peer instantly.
3. **Dial keys, not IPs.** Every device has an Ed25519 identity. Connections are end-to-end
   encrypted QUIC (via [iroh](https://github.com/n0-computer/iroh)) with NAT hole punching and
   relay fallback. Relays forward ciphertext only.
4. **No central server required.** Pairing works with a one-time ticket. An optional account
   service *(planned)* only distributes device keys and ACLs — like a Tailscale control plane —
   and never sees session traffic. Everything is self-hostable.
5. **One Rust core, native shells.** Networking, protocol, policy and capability providers live in
   Rust. Desktop and iOS apps are thin native UIs over the core.

## 3. System overview

```
 Controller side                                         Host side
┌───────────────────────────────┐                      ┌──────────────────────────────────────┐
│ MCP agent (Claude Code, ...)  │                      │ ai-remote host                        │
│        │ stdio MCP            │                      │  ┌─────────────┐   ┌───────────────┐  │
│ ┌──────▼──────────────────┐   │   iroh QUIC (E2EE)   │  │ authz:      │   │ capability    │  │
│ │ ai-remote mcp (bridge)  │◀──┼──────────────────────┼─▶│ peer allow- │──▶│ providers:    │  │
│ │  - host registry        │   │  direct (hole punch) │  │ list, perms │   │ screen, ui,   │  │
│ │  - MCP client per host  │   │  or via relay        │  │ approval    │   │ input, shell, │  │
│ └─────────────────────────┘   │                      │  │ audit log   │   │ files, system │  │
│ Human UI (desktop / iOS)      │                      │  └─────────────┘   └───────────────┘  │
└───────────────────────────────┘                      └──────────────────────────────────────┘
              ▲                                                         ▲
              └───────────── optional account service (planned) ───────┘
                         device keys + ACLs only, no session data
```

## 4. Identity, pairing, authorization

- **Identity**: each installation generates an iroh secret key on first run and stores it in the
  profile directory. The public key (endpoint id) is the device identity.
- **Pairing (M0, no server)**, provided by mcpmesh's pairing ceremony (see `ar-net`):
  1. Host owner runs `ai-remote pair-code` (or starts `ai-remote host` with nobody paired); the
     host mints a single-use invite for one permission level and prints it as a ticket line. A
     background host never mints one by itself: a ticket is a credential.
  2. Controller runs `ai-remote pair <ticket> --name <alias>`; it dials the host, proves the
     invite's secret, and prints a three-word **safety code** with the granted level.
  3. The host records the controller's principal in that level's allow list and shows the same
     safety code with the controller's name and level (in its terminal, and always in its log).
     Both owners compare the words; different words mean someone else redeemed the ticket, and the
     host owner removes that peer with `ai-remote forget <peer>`.
  Because the ticket contains the host's public key, the controller is guaranteed to talk to the
  intended host; because iroh authenticates both ends, the host learns the controller's real key.
- **Authorization**: a connection on the session ALPN from an endpoint id that is not in the
  host's trusted-peer list is closed immediately. Permission levels:

  | Level | Tools allowed |
  |---|---|
  | `observe` | read-only tools (system info, screenshot, UI tree, window list) |
  | `control` | + input and UI actions |
  | `admin` | + shell and file system |

- **Account service** *(planned)*: sign in, bind device keys to an account, share hosts with other
  accounts, push ACL changes. Hosts keep working with local pairings when the service is down.

## 5. Transport and wire protocol

- iroh endpoint per process, default n0 relays during development, custom/self-hosted relays
  configurable.
- ALPNs:
  - `ai-remote/pair/0` — pairing request/response (single JSON message each way).
  - `ai-remote/mcp/0` — one bidirectional stream per MCP session, newline-delimited JSON-RPC 2.0
    exactly as MCP stdio framing. The controller is the MCP client, the host is the MCP server.
  - `ai-remote/screen/0` *(planned)* — low-latency video for humans (hardware H.264/HEVC,
    adaptive bitrate) and input events.
- Versioning: the ALPN carries the major protocol version; MCP's own `initialize` negotiates the
  MCP spec version and capabilities.
- Refusals: a host that will not serve a device closes the connection with a code and reason
  (the transport's 401 for unknown or revoked devices, the host's 403 for "not paired" or "no
  access level", code 0 when the host stops). The controller maps them to plain language for the
  agent ("access revoked by the host", "not paired", "no access level", "host offline or
  unreachable", "the host closed the connection") with the fix, and does not redial into a refusal
  (`ar_net::refusal`). It remembers a revocation, because later dials only meet the gate's generic
  401.

## 6. Host tool surface (v0)

Tool names are stable API. Annotations follow MCP (`readOnlyHint`, `destructiveHint`).

| Tool | Level | Purpose |
|---|---|---|
| `system_info` | observe | OS, hostname, user, monitors (geometry, scale), host version |
| `doctor` | observe | Graded preflight (ok / warn / fail, reason, fix): session, console, desktop lock, elevation, DPI awareness, relay, firewall, autostart, approval prompts, audit log, disk |
| `screenshot` | observe | Capture a monitor, downscale (default max 1280 px wide), return image + coordinate mapping |
| `list_windows` | observe | Top-level windows: title, app, pid, bounds, focused/minimized |
| `ui_tree` | observe | Accessibility tree of the focused window (or a given window) with element refs, roles, names, values, bounds, states, supported actions |
| `ui_action` | control | Act on an element ref: invoke, set_value, toggle, expand, collapse, select, focus |
| `mouse` | control | move / click / double_click / right_click / drag / scroll at coordinates in the last screenshot's space |
| `keyboard` | control | type unicode text or press key combos (`ctrl+l`, `cmd+space`) |
| `focus_window` | control | bring a window to the foreground |
| `shell` | admin | run a command with timeout, capture stdout/stderr/exit code |
| `files_*` *(planned)* | admin | list/read/write/transfer files |

Coordinates: `screenshot` returns the mapping from image pixels to screen coordinates; `mouse`
accepts image-space coordinates by default so a model never has to do DPI or multi-monitor math.

## 7. Safety model

- **Permission levels** per peer (section 4).
- **Approval**: the host can require owner approval for `admin` tools (and optionally `control`).
  The prompt is a native dialog on the host's interactive desktop (the host often runs without a
  terminal, see section 8); when nobody can answer (locked desktop, no answer in time) the action
  is refused.
- **Audit log**: every tool call is appended to `audit.jsonl` on the host (time, peer, tool,
  argument summary, outcome, duration).
- **Revocation & kill switch**: `ai-remote revoke <peer>` refuses a peer and cuts its live
  connections, `ai-remote forget <peer>` also drops the pairing; stopping the host ends everything
  and closes every controller connection with a "host stopping" reason. A global hotkey and
  on-screen "AI is controlling" indicator are planned.
- **Prompt injection awareness**: on-screen text is untrusted data. Tool results that contain
  screen text are labeled as such so agents can treat them accordingly.

## 8. Process model and install

The host must run in the logged-in user's interactive desktop session: a service (session 0 on
Windows) has no desktop, so screenshots are black, input goes nowhere and no approval prompt can be
shown. So the host is never a service; it runs as one of two programs of the same build:

| Process | Started by | Output |
|---|---|---|
| `ai-remote host` | the user, in a terminal | terminal (stdout), logs on stderr; prints a pairing ticket when nobody is paired |
| `ai-remote-hostw` | the autostart entry, at login | no console window (Windows GUI subsystem); everything goes to `<profile>/host/host.log` (one rotated copy kept); never mints a ticket |
| `ai-remote agent` | the CLI or `ai-remote mcp`, on demand | `<profile>/controller/controller.log`; exits after 30 idle minutes |

Both host programs take the control endpoint of their profile, so only one host runs per profile.
Its `status` reports the pid (a restart is verified by a changed pid) and whether it runs in the
background.

**`ai-remote host install`** registers the background host (`--dry-run` prints every change,
including the full task definition; repeating it is safe and is the upgrade path):

- **Windows**, from an elevated terminal (the install checks and explains):
  - A Task Scheduler task `\ai-remote\host-<profile id>`, registered with `schtasks.exe` (absolute
    path) from an XML definition: logon trigger of the current user (by SID), interactive token,
    highest privileges (so the host can drive elevated windows), no execution time limit (the
    default stops tasks after 3 days), never stopped or blocked on battery, started late when the
    logon was missed, a second instance ignored. It runs `ai-remote-hostw --home <profile>`.
  - Inbound Windows Firewall allow rules `ai-remote host` (UDP and TCP, any profile) for both host
    programs, written with `netsh.exe`. The first listen of a new program otherwise raises an
    "allow access?" prompt on the desktop; unanswered on an unattended machine it leaves program
    block rules that beat allow rules and quietly push connections onto the relay. Stale block rules
    for the programs (found through the firewall's COM API) are deleted.
  - Unless `--no-start`, it starts the task and prints the host's own doctor report as a
    self-check, then the next step (pairing).
- **macOS**: a per-user LaunchAgent `~/Library/LaunchAgents/io.github.jia-ss.ai-remote.host.<id>.plist`
  (`RunAtLoad`, `KeepAlive` except after a clean exit so `stop-host` sticks, `ProcessType`
  Interactive, Aqua sessions only), loaded with `launchctl bootstrap`. No elevation.

`ai-remote host uninstall` stops the running host, deletes the task (or LaunchAgent) and the
firewall rules; `ai-remote host status` shows the entry, what it runs, whether that is this
installation and profile, the firewall rules per program, and whether the host runs.

**`ai-remote doctor`** asks the running host for its graded preflight (the same checks as the
`doctor` tool, seen from the host's own session, token and network); without a running host it
checks from the CLI's process and skips the network. It exits with 1 when a check fails.

## 9. Code layout

```
crates/
  ar-core/        profile layout, device key, permission levels, config, result budgets
  ar-net/         transport seam over mcpmesh-node: pairing (with the safety-code watch), gate,
                  revocation, dialing, refusal reasons, local control channel (ipc)
  ar-host/        host: gate (levels, policy, approval, audit), MCP server, tools, platform facts,
                  doctor, install (autostart + firewall)
  ar-controller/  controller agent, aggregating MCP server over all paired hosts, stdio shim
  ai-remote/      the `ai-remote` CLI and the windowless `ai-remote-hostw` background host
apps/             native shells (planned): desktop UI, iOS controller
docs/
```

Profiles: everything lives in a per-user profile directory; `AI_REMOTE_HOME` (or `--home`)
overrides it so several identities can run on one machine (used by tests and local development).
Paths are canonicalized without the Windows `\\?\` prefix, since they are shown to people and
handed to system tools.

## 10. Milestones

| Milestone | Scope |
|---|---|
| **M0** | Rust workspace; iroh identity + pairing; host MCP server over iroh with system_info, screenshot, mouse, keyboard, shell, list_windows, ui_tree/ui_action (Windows); controller `ai-remote mcp` bridge usable from Claude Code; audit log; tests |
| M1 | macOS host (ScreenCaptureKit, AX tree, CGEvent); file tools; approval UX; CI on Windows + macOS |
| M2 | Human controller UI with live video (`ai-remote/screen/0`), desktop app shell |
| M3 | iOS controller app (SwiftUI + Rust core via FFI), App Store submission |
| M4 | Optional account service + self-hosted relay, device sharing |
