# ai-remote desktop app

The controller app for people: see your computers, watch what an agent is doing, approve or deny risky
steps, and take over. It is a [Tauri 2](https://v2.tauri.app) app (React + TypeScript + Tailwind) whose Rust
side talks to the same per-user controller agent as the `ai-remote` CLI, so the app, the CLI and every MCP
client share one identity and one set of pairings.

Status: early. The main window (computers, the computer page with its Access and Tools tabs,
approvals, activity, pairing, first run, settings) and the live session (video, the capsule, the turn line, the decision card, take over and hand
back) follow the v2 design system. In a plain browser (`npm run dev`) the page runs on a demo backend
with sample computers; `?empty` shows the first run, `?view=empty&empty` the empty computer list after
it, `?view=session&host=office-pc` a session.

Debug builds can check the real app without showing it: `AI_REMOTE_SMOKE=out.png` runs it off screen
and saves a picture of its window (see `smoke` in `src-tauri/src/lib.rs`).

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

状态：早期。主窗口（电脑列表、电脑页及其“访问权限”和“工具”标签页、待批准、设置、活动记录、配对、首次启动）和实时会话（画面、驾驶胶囊、轮次线、
决定卡片、接管与交还）已按 v2 设计系统实现。在普通浏览器里（`npm run dev`）页面使用带示例电脑的演示后端；
`?empty` 显示首次启动，`?view=empty&empty` 显示首次启动之后的空电脑列表，`?view=session&host=office-pc` 显示会话。

调试版可以在不显示窗口的情况下检查真实应用：`AI_REMOTE_SMOKE=out.png` 会让它在屏幕外运行并保存窗口截图
（见 `src-tauri/src/lib.rs` 里的 `smoke`）。

```bash
npm install
npm run tauri dev
```

应用通过 `ai-remote` 命令启动控制端 Agent，查找顺序：`AI_REMOTE_CLI`、应用所在目录、`PATH`；开发时先
`cargo build -p ai-remote`。

Rust 命令的 TypeScript 绑定（`src/bindings.ts`）在每次调试启动和 `cargo test -p ai-remote-desktop` 时重新生成。
