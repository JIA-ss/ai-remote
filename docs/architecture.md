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
- **Pairing (M0, no server)**:
  1. Host owner runs `ai-remote pair-code`; the host creates a one-time pairing secret (128-bit,
     expires after 10 minutes, single use) and prints a ticket = host endpoint id + secret.
  2. Controller runs `ai-remote pair <ticket> --name <alias>`; it dials the host on the pairing
     ALPN and presents the secret.
  3. Host verifies the secret in constant time, records the controller's endpoint id with a
     permission level, and returns its own display name. The controller stores the host under
     the alias.
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

## 6. Host tool surface (v0)

Tool names are stable API. Annotations follow MCP (`readOnlyHint`, `destructiveHint`).

| Tool | Level | Purpose |
|---|---|---|
| `system_info` | observe | OS, hostname, user, monitors (geometry, scale), host version |
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
  M0 prompts in the host terminal; the desktop app will show a native prompt with the exact
  action.
- **Audit log**: every tool call is appended to `audit.jsonl` on the host (time, peer, tool,
  argument summary, outcome, duration).
- **Revocation & kill switch**: `ai-remote peers remove <id>` drops a peer and its live sessions;
  stopping the host ends everything. A global hotkey and on-screen "AI is controlling" indicator
  are planned for the desktop app.
- **Prompt injection awareness**: on-screen text is untrusted data. Tool results that contain
  screen text are labeled as such so agents can treat them accordingly.

## 8. Code layout

```
crates/
  ar-core/        identity, profile dirs, pairing tickets, peer store, iroh endpoint, ALPNs
  ar-host/        host service: authz, approval, audit, MCP server, capability providers
  ar-controller/  host registry, MCP client per host, aggregating stdio MCP bridge
  ai-remote/      the `ai-remote` CLI binary
apps/             native shells (planned): desktop UI, iOS controller
docs/
```

Profiles: everything lives in a per-user profile directory; `AI_REMOTE_HOME` overrides it so
several identities can run on one machine (used by tests and local development).

## 9. Milestones

| Milestone | Scope |
|---|---|
| **M0** | Rust workspace; iroh identity + pairing; host MCP server over iroh with system_info, screenshot, mouse, keyboard, shell, list_windows, ui_tree/ui_action (Windows); controller `ai-remote mcp` bridge usable from Claude Code; audit log; tests |
| M1 | macOS host (ScreenCaptureKit, AX tree, CGEvent); file tools; approval UX; CI on Windows + macOS |
| M2 | Human controller UI with live video (`ai-remote/screen/0`), desktop app shell |
| M3 | iOS controller app (SwiftUI + Rust core via FFI), App Store submission |
| M4 | Optional account service + self-hosted relay, device sharing |
