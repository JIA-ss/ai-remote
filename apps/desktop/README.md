# ai-remote desktop app

The controller app for people: see your computers, watch what an agent is doing, approve or deny risky
steps, and take over. It is a [Tauri 2](https://v2.tauri.app) app (React + TypeScript + Tailwind) whose Rust
side talks to the same per-user controller agent as the `ai-remote` CLI, so the app, the CLI and every MCP
client share one identity and one set of pairings.

Status: early. The screens follow the v2 design system and are being built now.

```bash
npm install
npm run tauri dev
```

The app starts the controller agent through the `ai-remote` command. It looks for it in `AI_REMOTE_CLI`,
next to the app, then on `PATH`; during development build it first with `cargo build -p ai-remote`.

TypeScript bindings for the Rust commands (`src/bindings.ts`) are regenerated on every debug start and by
`cargo test -p ai-remote-desktop`.

---

# ai-remote 桌面应用

给人用的控制端：查看你的电脑、看 Agent 正在做什么、批准或拒绝高风险操作、随时接管。它是一个
[Tauri 2](https://v2.tauri.app) 应用（React + TypeScript + Tailwind），Rust 一侧连接的是和 `ai-remote`
命令行同一个用户级控制端 Agent，所以应用、命令行和所有 MCP 客户端共用一个身份和同一套配对。

状态：早期。界面按 v2 设计系统制作中。

```bash
npm install
npm run tauri dev
```

应用通过 `ai-remote` 命令启动控制端 Agent，查找顺序：`AI_REMOTE_CLI`、应用所在目录、`PATH`；开发时先
`cargo build -p ai-remote`。

Rust 命令的 TypeScript 绑定（`src/bindings.ts`）在每次调试启动和 `cargo test -p ai-remote-desktop` 时重新生成。
