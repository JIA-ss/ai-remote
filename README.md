# ai-remote

<p align="right"><b>English</b> · <a href="README.zh-CN.md">中文</a></p>
<p align="center"><picture>
<source media="(prefers-color-scheme: dark)" srcset="assets/brand/home-dark.svg">
<source media="(prefers-color-scheme: light)" srcset="assets/brand/home-light.svg">
<img src="assets/brand/home-light.svg" width="880" alt="ai-remote: your agent works, you keep the controls. MCP remote control for Windows and macOS; experimental iPhone controller source.">
</picture></p>
<p align="center"><a href="#start-from-source">Start from source</a> · <a href="#connect-your-agent">MCP setup</a> · <a href="#your-computers-your-rules">Safety</a> · <a href="#what-works-today">Platform status</a> · <a href="docs/architecture.md">Architecture</a></p>

**Open-source remote computer control for MCP agents, with a desktop app for the person watching.**
Use Codex, Claude Code, Cursor, VS Code or another stdio MCP client to operate your paired Windows PCs and Macs. See the screen, send mouse/keyboard input, inspect windows, and run permitted tools over encrypted peer-to-peer connections. Pause the agent, take over, and hand back when you are ready.

> [!NOTE]
> **Pre-alpha, build from source.** The Mac host and desktop app have passed local acceptance on Apple Silicon. Windows has an existing implementation. The native iPhone controller source is present, with **device/simulator SDK builds and simulator startup/pairing UI tests passed**. Physical iPhone remote-control acceptance is still deferred. No signed releases or App Store app are available. [Validation](docs/iphone-validation.md)

## What works today

| Surface | Status | Start here |
| :-- | :-- | :-- |
| Windows host / CLI | Implemented; not revalidated on Windows in the latest Mac pass | [Operations](docs/operations.md) |
| macOS host | Pairing, screenshots, input and H.264 stream accepted locally | [Mac setup and limits](docs/macos.md) |
| Desktop controller | Computers, live view, activity, pairing, takeover and MCP-client setup | [Desktop app](apps/desktop/README.md) |
| MCP bridge | Real stdio initialization, tools/list and connected-client tracking verified | [Client setup](docs/mcp-clients.md) |
| iPhone controller | Experimental SwiftUI/UniFFI controller; iOS SDK builds and simulator UI tests passed | [iPhone build and acceptance](apps/ios/README.md) |

The Mac acceptance used macOS 27 on Apple Silicon. Intel Macs and older OS versions need their own runtime checks. The development video stream is functional but currently low-rate; it is not a latency or FPS benchmark.

## Why use ai-remote

- **Bring your existing agent.** One MCP bridge reaches the computers you paired; the project does not require its own hosted AI model.
- **Pair by device identity.** One-time tickets establish trust. QUIC uses direct paths when available and encrypted relay fallback when needed.
- **Give only the access needed.** Each device is observe, control or admin. The host checks permission, turn, policy and approval before a tool runs.
- **Watch and intervene.** The human app shows live video and an activity feed. Taking over stops the agent's control calls until you hand back.
- **Extend through existing MCP servers.** Mount a local server with a pinned executable and an explicit tool allowlist. Accessibility-tree tools come from such integrations; a universal native tree is not included.

<p align="center"><picture>
<source media="(prefers-color-scheme: dark)" srcset="assets/brand/how-it-works-dark.svg">
<source media="(prefers-color-scheme: light)" srcset="assets/brand/how-it-works-light.svg">
<img src="assets/brand/how-it-works-light.svg" width="880" alt="An MCP client connects to ai-remote mcp, which reaches paired computer hosts over encrypted QUIC; the host enforces levels, approval and audit.">
</picture></p>

## Start from source

You need the pinned Rust toolchain (1.95.0). The desktop app also needs Node/npm and the platform's native development tools.

```sh
git clone https://github.com/JIA-ss/ai-remote.git
cd ai-remote
cargo install --path crates/ai-remote --locked
```

On **macOS**, build and open a development app with its CLI included:

```sh
tools/macos.sh build --debug --bundles app
open target/debug/bundle/macos/ai-remote.app
```

On the **computer to control**, run the installed CLI (make sure Cargo’s bin directory is on PATH):

```sh
ai-remote host
# In another terminal on that computer:
ai-remote pair-code --level control
```

On the **controller computer**, redeem the one-time ticket:

```sh
ai-remote pair <ticket> --name office-pc
ai-remote call system_info --host office-pc
```

Compare the same three-word safety code on both devices. On a Mac host, grant Screen Recording and Accessibility to the actual running executable, restart it, and run `ai-remote doctor`. A locked or unauthorized desktop must be repaired before capture/input, not worked around. [Detailed operations](docs/operations.md)

## Connect your agent

In the desktop app, open **Settings → Agents**, choose your MCP client, then copy its setup. The app supplies the actual executable path and the current pairing profile, including when the CLI lives inside the Mac bundle.

If `ai-remote` is on PATH, the basic commands are:

```sh
# Claude Code, available across your projects
claude mcp add --transport stdio --scope user ai-remote -- ai-remote mcp
# Codex
codex mcp add ai-remote -- ai-remote mcp
```

Cursor uses `mcpServers` JSON in `~/.cursor/mcp.json`. VS Code uses `servers` JSON in its MCP configuration. Codex also supports `[mcp_servers.ai-remote]` in `~/.codex/config.toml`. Paths, quoting and exact examples: [MCP client setup](docs/mcp-clients.md).

## Your computers, your rules

| Level | Allowed tools |
| :-- | :-- |
| `observe` | Screen, window list, system info and permitted read-only integrations |
| `control` | Observe plus mouse, keyboard and clipboard |
| `admin` | Control plus shell and background jobs; risky calls ask first |

High-risk steps wait for owner approval. An unanswered request is denied. Every decision and outcome goes to the host's audit log. Revoke/unpair a device to remove its access.

**Allowing remotely currently requires Windows Hello.** Mac and iPhone controllers can deny; their Touch ID / Face ID signing backends are not implemented. The native host prompt remains an approval path when the host's desktop is usable. [Approval design](docs/approvals.md)

## Questions people ask

**Can my iPhone control my computer?** That is the purpose of `apps/ios`: native SwiftUI UI over the shared Rust controller. The source includes pairing, video, touch/text, takeover and activity. Device and arm64 simulator builds pass with the iOS SDK, as do simulator startup, invalid-ticket, background/resume and relaunch tests. Live control on a physical iPhone still needs acceptance.

**Can an agent control my iPhone through this app?** No. The iOS source is a controller, not an iPhone host.

**Do I need port forwarding or an ai-remote cloud account?** No manual port-forwarding or project cloud account is part of normal pairing. Network restrictions can still make a peer unreachable; the tool reports that rather than guaranteeing every network works.

**Does it include a native accessibility tree or file manager?** The core provides screenshots, input and windows; allowlisted mounted MCP servers provide additional tools. Native Mac accessibility-tree and file-transfer work remain planned.

**Where are the binaries?** No signed release is published yet. Build from source; Windows, Intel Mac, iPhone signing and notarization have separate verification needs.

## Development and evidence

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
node --test tools/mcp-install.test.mjs
```

[Mac acceptance](docs/macos.md) · [iPhone validation ledger](docs/iphone-validation.md) · [Tool reference](docs/tools.md) · [Security](SECURITY.md) · [Contributing](CONTRIBUTING.md)

## Roadmap

| Stage | Next work |
| :-- | :-- |
| M0–M2 | Continue Windows/Mac acceptance, improve capture performance, complete file/accessibility work and signed packaging |
| M3 | Physical phone/remote-session acceptance; Face ID signing, QR pairing and notifications |
| M4 | Optional self-hosted account/relay services and device sharing |

## License

[AGPL-3.0](LICENSE). The official project is [JIA-ss/ai-remote](https://github.com/JIA-ss/ai-remote).
