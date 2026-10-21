# AGENTS.md

给接手开发的人和 coding agent 的工作笔记。用户文档在 `README*.md` 和 `docs/`；这里只记**当前在做什么、为什么这么做、下一步是什么、踩过的坑**。每次推进后更新本文件，并在提交说明里写清思路。

## 通用约定

- 工具链固定 Rust 1.95.0（`rust-toolchain.toml`），`rustfmt.toml` 行宽 120。提交前：
  `cargo fmt --all`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`、
  `node --test tools/desktop-state.test.mjs tools/manual-input.test.mjs tools/stream-ui.test.mjs tools/mcp-install.test.mjs tools/chat-state.test.mjs`、
  `cd apps/desktop && npx tsc --noEmit -p .`。
- CI（`.github/workflows/`）在 `macos-15` 和 `windows-2025` 上跑 Rust 测试/Clippy/前端测试；iOS 有单独的 workflow。
  Mac/iOS 代码只能在 Mac 或 CI 上编译，Windows 上改了 `cfg(target_os = "macos")` 的代码要靠 CI 兜底。
- 换行符：`.gitattributes` 是 `* text=auto eol=lf`，提交时统一转 LF。Windows 上用脚本改文件优先二进制读写
  （`open(p, 'rb')` / `open(p, 'wb')`）。检查换行用 `git ls-files --eol <file>`；Git Bash 里 `grep -c` 查 CR 会误报。
- Windows 上的 Bash 工具对很长的 heredoc（含引号的大段代码）解析不稳定，大段内容先写成文件再处理。
- 文档写数字时分清"实测（条件、样本量）/ 推导 / 未测"，不要把请求值写成实测值。
- `skills/ai-remote/SKILL.md` 是给 agent 看的操作方法（会话起手、看 / 操作 / 命令 / 文件、级别与审批、拒绝提示对照、机主接管、
  命令行速查）。改工具参数、命令行选项或拒绝文案时同步改它；README 两种语言和 `docs/mcp-clients.md` 指向它。

## 被控端编程智能体与对话侧栏（2026-10-21，分支 `feat/host-chat`）

用户需求：在主控端列出被控端装了哪些编程智能体 CLI（Claude Code、Codex、OpenCode 等，要同时支持多种），一键以 bypass 方式在被控端启动，在推流画面旁边用对话窗口实时交互。用户文档：`docs/tools*.md` 5.15、`docs/architecture*.md` 的 `chat/1`、`docs/operations*.md` 4.1 表格、`skills/ai-remote/SKILL.md` 7b。

### 调研结论

- 参考了 multica 和 Orca 两个项目：
  - multica：Go daemon，每个 CLI 一个原生适配器，带一张检测表；
  - Orca（stablyai/orca）：多端，"会话日志 + 序号游标"续传，原生协议与 PTY 并存。
- 两者都**只借思路，没有抄代码**：multica 的许可证是 Apache-2.0 加了限制嵌入商用的附加条款，与 AGPL 不兼容；Orca 是 MIT，按思路借用。
- 选型是"原生适配器 + ACP 词汇的统一事件流"。没有选"全走 ACP"：Claude 和 Codex 的 ACP 桥都需要 Node ≥22 和额外的 npm 包，而且版本与用户本机装的 CLI 对不上。也没有选"PTY 透传"：它给不出"对话窗口"，审计也只能记到字节。

### 设计

- **被控端** `crates/ar-host/src/chat/`：
  - `runtimes.rs`：数据化的描述表，加上 `chat.toml` 里的自定义条目（例如另一个 `CLAUDE_CONFIG_DIR`）；
  - `detect.rs`：查找顺序为自身 PATH → 登录 shell（一次 `$SHELL -ilc`，先 unalias，并带回 `$PATH`）→ 安装器常用目录；之后跑 `--version` 取版本，并查登录态；
  - `proc.rs`：进程树，Unix 用 ProcessSession，Windows 用 Job Object 加 `CREATE_NO_WINDOW`；prompt 只走 stdin；单行上限 32 MiB；
  - `adapters/{claude,codex,acp}.rs`：三种协议的适配器；
  - `mod.rs`：ChatManager。每个会话一个 driver 任务，负责排队、打断、parked 后续接、空闲回收、重启时回收孤儿进程，以及按 id 去重重复消息。
- **事件**：先在被控端编号、写盘，再发出。重连时带 `since`，被控端补发之后的事件；观看者跟不上实时流时，改从磁盘补。
- **工具与通道**：工具有 `chat_runtimes`、`chat_sessions`、`chat_start`（默认询问）、`chat_attach`、`chat_interrupt`、`chat_end`。对话走 ALPN `app/ai-remote/chat/1`，用法和 files 一样"工具签票 + 专用通道"。`chat_start` / `chat_attach` 加进了 `CHANNEL_TOOLS`，所以 MCP 客户端看不到。
- **闸门之外**：
  - 本机总开关 `ai-remote chat enable|disable`：只能在本机设置，对 bypass 设备也生效；
  - 设备失去 admin 后 2 s 内结束它发起的会话（`chat_channel::enforce`）；
  - pause 会停掉正在跑的轮次；take_over **不会**（智能体不用鼠标键盘，接管的人往往就是正在对话的人）；
  - 审计只记 `chat_session` / `chat_turn` 的元数据，不记正文；
  - `doctor` 新增 `chat` 检查项。
- **SYSTEM 或 root 身份的被控端**（Windows 服务模式、macOS 登录窗口）：`can_start: false`，不执行任何探测。原因是智能体必须以用户身份运行；用 SYSTEM 身份去执行用户目录里的程序，还等于本地提权。
- **主控端**：
  - agent 加 op `chat`，中继逻辑照抄 Screen；
  - 命令行：`ai-remote chat list|sessions|start|run|attach|send|interrupt|end|enable|disable|status`；
  - Tauri 命令：`open_chat` / `chat_send` / `close_chat` / `local_host_chat` / `set_local_host_chat`；
  - 桌面端：电脑页新增「智能体」标签；会话页右栏新增「对话」标签，打开时右栏加宽到 `clamp(380px,34vw,560px)`；设置 → 本设备里加开关。前端状态在 `lib/chat.ts`（纯函数 `reduceChat`，按帧批量合并），界面在 `ui/AgentChat.tsx`。

### 实测（本机 macOS，2026-10-21）

- **协议 spike**（手动驱动，`/tmp` 下执行）：
  - Claude 同一进程连续写多条 user 帧，每帧各出 `system/init` 和 `result`；`control_request interrupt` 立即收到应答。
  - Codex 0.163 在同一 thread 上两轮都记得上文；`turn/interrupt` 4 ms 生效；换新进程后 `thread/resume` 上下文仍在。
- **`tools/chat-smoke.py`**：本机隔离的被控端和控制端，走真实 QUIC，用假 Claude 跑完全部 21 项。
  - `--real codex=…/codex(0.163) --model gpt-6.1-sol`：一次任务 5.5~9.4 s；能跑 shell 工具；打断 0.1 s 生效；下一轮记得上文（3 次运行）。
  - `--real opencode=…(1.19.3) --model opencode/deepseek-v4.1-flash-free`（ACP）：4.0~7.9 s；工具、打断 0.3 s、续上文都通过（2 次运行）。
- **单元测试**：`cargo test -p ar-host --lib chat` 共 26 项，三个适配器用真实行格式的样例覆盖；`node --test tools/chat-state.test.mjs` 共 9 项。
- **Windows（win-dev，Windows 11，debug 构建）**：
  - `cargo clippy --workspace --all-targets -D warnings` 没有警告，`cargo test --workspace` 全部通过，其中 ar-host 255 项；新加的 Job Object 测试证明，停掉智能体时它拉起的子进程也会一起结束。
  - `tools/chat-smoke.py` 本机隔离被控端和控制端，假 agent 加真实 Codex 0.162.1（npm 装的 `codex.cmd`，验证了 shim 路径），25 项全部通过：一次任务 9.5 s，打断 0.3 s 生效，下一轮仍记得上文。被控端是终端前台运行的，不是计划任务里的 hostw。
- **桌面端**：在演示模式下，用浏览器跑通了"电脑页启动 → 会话页对话标签 → 流式步骤 → 停止"，以及 diff / 失败工具 / 思考的渲染。没有在真实 Tauri 应用里对真实被控端试过。

### 踩过的坑

1. 本机 `claude auth status` 显示 `loggedIn: true`，但 OAuth 已过期，`-p` 每轮都报 401，而且不会自动刷新。所以"已登录"只能当提示，真正的失败要靠 `result` 的 `is_error` 和 `api_error_status` 报出来。
2. codex 0.137 的默认模型（本机 config 里是 `gpt-5.5`）已经不被 ChatGPT 账号接受，每轮都在 `turn/completed` 之前报 400。适配器把 `error` 通知里的 message 作为本轮失败原因；表单里有模型字段。
3. serde_json 的 Map 会按字母排序 key，事件信封如果先转成 Map 再序列化，`seq` 就不在开头，`line_seq` 解析不到。改成手工拼字符串。
4. Codex 会把长命令转到后台，然后自己结束这一轮。测试打断时必须实时跟随事件、一看到工具调用就打断，靠轮询来不及。
5. React dev 模式会执行两次 effect，一次性票据在第一次挂载时就用掉了。前端在票据失败后改用 `chat_attach` 重新取票。
6. 文案键 `ag_*` 已经被"已连接的 MCP 客户端"那部分用掉了，新功能改用 `hc_*`。
7. macOS 的临时目录路径太长，超过 Unix socket 路径上限（SUN_LEN），冒烟脚本改放到 `/tmp` 下。

### 下一步

- [ ] Windows 计划任务（后台 hostw）模式下的真机对话、子进程是否弹控制台窗口：尚未验证。终端前台被控端的情况已经测过，见上面的 Windows 实测。
- [ ] 服务模式 chat-runner：SYSTEM 被控端以用户令牌拉起 `hostw --chat-runner`，同时解决 Win11 锁屏杀掉 hostw 后会话丢失的问题。
- [ ] Claude 真机端到端：要先在被控端重新登录 Claude（本机 token 已过期）。
- [ ] iOS 只读会话视图；Codex `turn/steer` 和 Claude hooks 的运行中插话；可选的 `kind:"pty"` 原始终端。
- [ ] 是否向 MCP 客户端开放 `chat_send` / `chat_read`（"AI 指挥 AI"），等用户决定。

## 桌面端视觉重做（2026-10-21，分支 `claude/desktop-ui-polish-4212f0`）

用户对 debug 包的评价是"太丑了，审美停在上个世纪，而且没有全屏显示"，要求做成一流 macOS 原生工具的样子（无边框、
现代、克制），并给会话页加沉浸全屏。调研结论、设计原则和令牌表在 `docs/desktop-design.md`；改前 / 改后截图在
`docs/images/desktop/`（`before-*` 是旧版，`after-*` 是本轮）。

### 决定

- **令牌名不动、取值重定义；覆盖层推倒重来。** 生成的 `tokens.css / base.css / v2.css / v2-pages.css` 一行没改
  （生成器不在仓库里），`liquid-glass.css` 删除，`native.css` 是唯一的覆盖层（加载最后），新 class 以 `nt-` 开头。
  两套中性灰（深 / 浅）、一个强调色（智能体蓝，也是选中和主按钮）、"你"的琥珀色保留为语义色、状态色在线绿 / 空闲灰 /
  中继橙 / 告警红。没有发光、光晕、噪点、壁纸，卡片不描边。
- **外壳**：侧栏 240 px 贴左、整高，顶部 48 px 拖拽条给红绿灯；内容列自带 48 px 标题条（`TitleBar` 多了 `end` 插槽）。
  macOS 上窗口 `transparent` + `windowEffects: ["sidebar","windowBackground"]`，侧栏透出桌面材质，内容区 CSS 画成不透明；
  主题被钉住（非"跟随系统"）或"减少透明度"时侧栏改画 `--canvas`（`data-theme-choice` / `data-transparency`）。
- **会话页**（`Session.tsx`，推流 / 解码 / 指针 / 输入逻辑没动）：黑色舞台边到边，画面按比例 contain 铺满短边（信箱只在一侧），边缘按 `devicePixelRatio` 取整到设备像素；底部浮条（状态圆点 + 短文案"无人操作 / 智能体操作中 / 你在操作"，接管 / 交还是普通尺寸强调色按钮，
  不显示设备 id；窗口态和全屏态同一套 3 s 自动隐藏）；右上 HUD（尺寸、fps、Mbps、
  延迟、线路，等宽数字）；时间线移到右侧日志栏顶部；待批准横幅在舞台顶部。`.stream-metrics` 这个 class 保留在 HUD 上，
  smoke 模式和 `tools/stream-e2e.py` 读它。
- **沉浸全屏**（`lib/fullscreen.ts`）：Tauri `setFullscreen`，权限 `core:window:allow-set-fullscreen / allow-is-fullscreen`。
  ⌃⌘F 和标题条按钮进入；Esc 只在"观看"时退出（接管后 Esc 要发给远端）；绿灯按钮进的原生全屏靠 `onResized` 后 0.3 / 1 / 2 s
  三次 `isFullscreen` 同步；自己触发的切换 1.5 s 内忽略监听结果（否则过渡期间读到旧值会把状态翻回去）。全屏下浮条在
  鼠标移动时出现、3 s 无操作隐藏；接管状态下只有靠上下边缘才出现（浮条压着远端像素）。
- 动效只有浮条进出场（motion `AnimatePresence`，180 ms，8 px 位移）。

### 踩过的坑

1. `tauri.conf.json` 里窗口材质字段叫 `windowEffects`，不是 `effects`（写错 Tauri 直接拒绝配置）。
2. **WebKit 丢掉两个带 `backdrop-filter` 的浮层之一**：顶部 / 底部浮条同时做 transform 动画时，macOS 26 上只画出一个，
   另一个要等下一次重绘。浮条改成实色（`rgb(28 28 30 / .96)`），HUD 也去掉模糊。
3. `left: 50%` 的绝对定位锚点只能拿到容器一半的可用宽度，里面的文字会被省略；锚点要 `width: max-content`，
   并且**不要**再给子元素百分比 `max-width`（和 max-content 父元素循环，又会把文字挤掉）。
4. `tools/stream-ui.test.mjs` 对 `Session.tsx` 的 `require` 列表是白名单，新依赖（`motion/react`、`../lib/fullscreen`）
   要加桩；它跑在没有 `window` 的 vm 里，所以夹具里补了 `window`。
5. 截图：`screencapture -l` 在本机没有权限；computer-use 的窗口截图（`app_screenshot`）对透明窗口时好时坏，稳定的是
   整屏截图再按窗口位置裁（`tools` 里没放脚本，裁剪参数见 PR）。

### 实测（本机 macOS，debug 包，被控端 win-dev 5120×2160 极致档）

- 四个主页面 + 电脑页 + 设置页深 / 浅色都截了图，侧栏材质、无边框卡片、HUD 和浮条按设计显示。
- 会话页：画面边到边、HUD 显示 5120×2160 · 75~81 fps · 15.5~16 Mbps · 17~19 ms · 直连；浮条文字不再截断。
- 全屏：均衡档（1920×810 / 60 fps）下进入 2.5 s 内页面就是最终布局（顶部浮条、底部浮条、HUD、画面铺满宽度），3 s 后浮条
  自动隐藏，Esc 退出立即回到窗口态。**极致档（5120×2160 / 75 fps）下进入全屏后页面要 5~10 s 才跟上**（画面停在旧尺寸、
  顶部浮条晚出现、退出后 chrome 晚回来），一有鼠标事件就追上；不透明窗口一样。原因是全屏时画布变成 5120×2160 设备像素，
  每帧 `drawImage` 把合成器喂饱了，UI 重绘排不上队；不是状态机的问题（试过 `setFocus` 唤醒，没用，已去掉）。

### 下一步

- [ ] 极致档全屏下 UI 重绘滞后 5~10 s：画布 5K@75fps 的 `drawImage` 压满合成器。可选方案：全屏时把画布交给
      `OffscreenCanvas` + worker，或在 UI 层级变化时临时降帧（`setBitrate` 之外还没有降帧的控制行）。
- [ ] Windows 端没验证（本机只有 Mac）：`tauri-plugin-frame` 的标题栏高度仍是 48，`[data-os="windows"]` 下的字体和
      标题条布局要在 Windows 上看一眼。
- [ ] 设备卡片的悬停态、侧栏行的键盘焦点环还是生成层的样式，没有专门过。
- [ ] 待批准 / 活动记录在有数据（决策卡、收据）时的样子只在演示模式看过，没有真实数据截图。

## 命令行远程操作（2026-10-12，分支 `feat/cli-remote`）

目标：人、脚本和 CI 不经 MCP 也能直接操作被控端，形态参照内部测试机工具的命令行：
`ai-remote exec / job / shot / windows / info / doctor --host / ls / stat / mkdir / rm / push / pull`，
默认被控端，统一的退出码和 `--json`。用户文档：`docs/operations*.md` 4.1 节、`docs/tools*.md` 5.14 节。

### 设计

- **每条命令都是工具调用**（经常驻进程的 `call` op），不开旁路：级别、策略、审批、审计、轮次对命令行和 Agent 一视同仁。
  命令行代码在 `crates/ai-remote/src/remote.rs`，`main.rs` 只挂子命令。
- **文件传输两步走**：`files_push` / `files_pull` 工具过闸门后签一张一次性票据（绑定设备、路径，上传另绑大小和
  SHA-256，2 分钟有效）；字节走新 ALPN `app/ai-remote/file/1`（架构文档原本就规划了这个名字），不经 MCP——MCP 结果
  预算约 1 MiB，且分块调用会让 ask 级工具每块弹一次审批。被控端 `crates/ar-host/src/files_channel.rs`，
  工具 `tools/files.rs`；控制端常驻进程新 op `files`（`agent.rs` 的 `open_files` / `FileTransfer::relay`）。
- **双方按事先知道的大小读写，不靠关闭半条流**：Windows 命名管道不支持半关闭，命令行 ↔ 常驻进程这一段做不到 EOF。
- 上传写到目标旁的 `.<名>.ai-remote-<id8>.part`，大小和哈希都对才 `rename`（Windows 上 `std::fs::rename` 会替换已有文件）；
  `overwrite=false` 时 rename 前再查一次目标。下载在字节后附一行 SHA-256，命令行写本地 `.part`，核对后才改名。
- 兑换票据时再查级别（须 admin）和轮次；接管 / 暂停会中途停掉传输。每次传输一条审计 `transfer`（桌面端时间线忽略未知
  kind，不受影响）。
- `files_push` / `files_pull` 不出现在 MCP 工具列表里（Agent 拿到票据也用不了），MCP 调用会被拒并提示用命令行；
  `files_list / stat / mkdir / delete` 对 Agent 可见。
- **默认被控端**存在 `controller/hosts.json` 的 `default`，命令行和 MCP 会话共用（`Aggregator::implicit_host`，
  原 `only_host`）；`unpair` 那台时自动清掉。另有 `AI_REMOTE_HOST`（clap `env` 特性）作用于所有控制端 `--host`，
  `doctor` 除外（它不带 `--host` 时检查本机）。
- 退出码：0 完成；2 被控端拒绝或工具失败；1 调用没发出。`exec` 照 ssh：远端退出码原样、超时 124、没执行 255；
  `job wait`：任务退出码、超时 124、被杀 137、未知 255。
- `files_delete` 拒绝根目录，以及被控端配置目录本身和包含它的文件夹（删了身份和审计日志就没了）。

### 验证

`python tools/cli-remote-smoke.py [--cli <exe>] [--big-mib N]`：本机隔离的被控端 + 控制端（真实 QUIC，admin 级，
被控端规则放行 shell/jobs/files 但拒绝 `files_mkdir` 以覆盖拒绝路径），逐条跑命令并核对退出码、输出、哈希、审计记录、
MCP 工具列表。

### 实测（2026-10-12，本机，Windows 11，release）

- 冒烟 39 项：release 4 轮全过；debug 3 轮全过（其中最后加的接管测试只在第 3 轮）。覆盖：exec 退出码 / 超时 124 / stdin / env；job log -f、wait 透传码、
  wait 超时 124、kill；截图（PNG 超预算时被控端改发 JPEG，命令行按实际格式改扩展名——第一轮就是这里出错）；
  策略拒绝退出 2；push 新建目录 / 拒绝覆盖 / --force / 推到已有文件夹 / 相对路径拒绝；pull 拒绝覆盖本地 / 缺文件；
  无 `.part` 残留；ls / stat / rm -r / 配置目录保护；hosts default；doctor --host；MCP 隐藏两个签票工具；审计 `transfer`；
  **传输中 `take-over`**：上传在中途停下，被控端的原因完整回到命令行，目标和临时文件都不留。
- 吞吐（512 MiB 随机数据，本机回环 QUIC，含两端 SHA-256 和目标 fsync；每档 2 次，**样本少**）：
  转发缓冲改 1 MiB 前 push 90~92 / pull 98~111 MiB/s，改后 push 100~108 / pull 120~129 MiB/s。
  这是 ai-remote 自身开销的上限，**不是**局域网实测；千兆网（约 112 MiB/s）下大致接近线速，万兆下它会是瓶颈。
  剩余瓶颈未定位（推测在回环上的 QUIC 加密 / 流控，未验证）。

### 踩过的坑

1. 共享工作区里同时有别的会话在改推流代码；本分支在独立 worktree（`../ai-remote-cli`）里做，避免互相 `git add` / 编译干扰。
2. 工作区文件可能是 CRLF（`git ls-files --eol` 显示 `w/crlf`），按 LF 写的替换脚本会静默不命中；改已有文件优先用编辑工具。

### 下一步

- [ ] MCP 侧的 push/pull（控制端工具，读写控制端本机文件）——是否给 Agent 开放需要先定权限模型。
- [ ] 文件夹传输（现在要先打包）、断点续传。
- [ ] 桌面端审批卡给文件工具专门的问句（现在走通用"想调用 X"）。
- [ ] 传输吞吐：定位回环 100~130 MiB/s 的瓶颈（QUIC 窗口 / 加密 / 单流），在两台机器的局域网上实测。
- [ ] `shot` 只能拿到结果预算内的图（长边 ≤ 2576、约 762 KiB）；要原尺寸无损截图可走文件通道另开一条路。

## Bypass：完全信任控制端（2026-10-12，同在 `feat/cli-remote`）

用户需求："加一个 bypass permissions 模式，用户可以设置，设置之后 agent 默认权限都允许操作"。和用户确认过的取舍：
**全部放开**（不只跳过审批，级别和 deny 规则也不管）；**在设备的设置里控制**（按控制端逐台设）；被控端本身没有界面，
所以放在被控端那台机器的桌面应用"设置 → 本设备"里，再加命令行 `ai-remote bypass`。用户文档：`docs/operations*.md` 6.1 节。

### 设计

- 状态：`crates/ar-host/src/bypass.rs`，按控制端 principal 存 `<配置目录>/host/bypass.json`，可选 `until_ms` 到期；重启保留。
- **只能在被控端本机设置**：本地控制通道 `HostRequest::Bypass / SetBypass`（命令行和桌面端都走它），控制端没有任何请求能开它，
  Agent 经 ai-remote 给自己开不了。已有 `shell` 的 Agent 能改文件——和改被控端任何配置一样，写进了 approvals 威胁模型。
- 生效点（都用 `HostState::effective_level` 或 `bypass().covers`）：闸门按 admin 查级别并跳过策略/审批（`gate.rs`，同一次调用只判断
  一次 `bypassed`，级别和策略不会不一致）；工具列表（`server.rs`）；文件通道（`files_channel.rs`）；events 通道的 turn/tell 权限。
  `system_info.you`、`access`、feed hello 带 `bypass`，但 `level` 仍报真实级别。
- 仍然有效：配对和"有某个级别"（无级别的设备在连接层就被拒，bypass 不会给它访问）；接管/暂停（轮次检查在闸门里照旧）；审计
  （决策记录带 `bypass: true`，开关本身写 `kind: "bypass"`）。
- 自动结束：`revoke`、`level none`、`forget`、控制端自己 `lower` 都会清掉 bypass——收紧访问的动作不该被一个遗留开关抵消。
- `doctor` 新增 `bypass` 检查：开着就 warn。
- 审计决策仍记 `decision: "allow"` 加 `bypass: true` 字段，而不是新的 decision 值：桌面端时间线把 allow/approved 以外的都当成被拒。
- 桌面端：Tauri 命令 `local_host_bypass` / `set_local_host_bypass` 直连本机被控端的控制管道（同一配置目录）；设置页用下拉框
  （设置页没有开关组件，其他选项也都是下拉框），选"全部放开"弹红色确认框写明后果；电脑页"权限"标签对被信任的设备显示红字说明。
  `bindings.ts` 手写后由 debug 构建重新生成核对。

### 多端

- Windows / macOS 桌面端：被控端那台的"设置 → 本设备"在 host 运行时显示"正在运行"和逐台信任设置（不再显示安装命令）；
  控制端的电脑页顶部事实栏和"访问权限"标签标出"全部放开"（feed hello 的 `bypass`），bypass 下不再显示"只有管理权限能看到其他设备"。
  审批卡和标题给 `files_*` 写了专门的问句。macOS 走 Unix socket 控制通道，代码不分平台，由 CI（macos-15）编译和测试。
- iPhone：`PhoneStore.bypass`（access 应答和 hello），`mayControl` 包含 bypass，否则 observe 级的手机在 bypass 下"接管"会被错误置灰；
  权限标签显示"全部放开（无需审批）"。Swift 只能在 Mac 上编，靠 iOS CI。

### 验证

- `python tools/bypass-smoke.py`：observe 级控制端 + 无审批框 + deny 规则的被控端，21 项：开启前 shell 被级别拒；开启后 shell 免审批、
  deny 规则失效、文件传输可用、MCP 工具列表出现 shell/jobs/files；`info`/`access`/doctor/审计都有标记；接管仍能拦；关闭后恢复拒绝；
  `--all --for 3s` 到期自动失效；`level none` 清掉 bypass。
- 桌面端：演示后端下在浏览器里点过整个流程（下拉 → 确认框 → 生效、红字说明）；真实被控端下用 debug 应用的 smoke 截图核对列表。

## 方向调整：AI 走工具、人走画面，测试机默认全信任（2026-10-13，分支 `feat/trust-default`）

和用户讨论后定下的定位（记在这里，免得再跑偏）：

- ai-remote 相对内部测试机工具和 RustDesk 的**唯一差异点**是"人和 AI 共用一条配对连接"：人看推流、AI 调工具，
  级别 / 策略 / 审批 / 审计对两者一视同仁。推流只要"看得清 AI 在干什么"，不追观感；精力放在工具面和权限闭环上。
- AI 的工具面以内部测试机工具的形状为准（exec / job / 文件 / 截图 / 窗口 / 进程），**只走 ai-remote 的 QUIC 连接**，
  不另起 HTTP agent；不做兼容层、不改工具名，也不接游戏侧的 MCP。命令行（`ai-remote exec/job/ps/kill/...`）就是人和脚本的入口。
- 权限：自己的测试机**默认全放行**（下面的 bypass default），发现不对再手动收。日常电脑保持逐台 bypass 或审批。

### 这次做的

- **bypass 默认开关**（`crates/ar-host/src/bypass.rs`）：`bypass.json` 新增 `default: bool` 和 `except: [...]`。
  `coverage(peer)` 的优先级：本设备条目（`bypass on <设备>`）> 默认关 → 不覆盖 > 在 `except` 里 → 不覆盖 > 默认覆盖。
  `clear(principal, name, except)`：默认开着时关掉某台（`bypass off`、`level none`、`revoke`、控制端自己降级）进 `except`；
  `forget` 传 `except=false`，重新配对从默认开始。`bypass default on` 清空 `except`（"重新信任所有人"）。
  控制通道加 `SetBypassDefault { on }`；`Bypass` 应答多了顶层 `default` 和每台的 `how`（`device | default | excepted | no`），
  `bypass` 字段改为**实际生效**值（桌面端按它显示，不用改）。审计 `kind: "bypass"` 带 `default: true/false`。
  `doctor` 默认开着时 warn 并列出例外。命令行：`ai-remote bypass default on|off`，`ai-remote bypass` 列表标 `(default)` / `turned off`。
- **进程工具**（`crates/ar-host/src/tools/process.rs`）：`process_list`（admin、allow、只读；按名字子串 / pid 过滤，不返回命令行和环境）、
  `process_kill`（admin、默认 ask；pid 或全名，默认连子树）。杀法复用 `jobs.rs` 的 `ProcTable` / `kill_process`
  （pid + 启动时间钉住，Windows 上先 OpenProcess 再核对再 TerminateProcess），为此把 `Proc` / `Root` / `ProcTable` 放成 `pub(super)`。
  拒绝杀被控端自己、它的父进程（监护）、pid 0/4。命令行 `ai-remote ps [名字] [--pid N]`、`ai-remote kill --pid N | --name X [--no-tree]`。
- 文档：`docs/tools*.md` 加 5.13 进程、文件改成 5.14；`docs/operations*.md` 4.1 表加 ps/kill，6.1 加"默认信任"。

### 验证（2026-10-13，本机，Windows 11，debug）

- `cargo test -p ar-host`：bypass 3 项（含默认 / 例外 / forget 语义）、process 3 项（按 pid / 名字列出；拒绝杀自己和不存在的 pid；
  杀 `cmd /C ping` 树后父子都没了）。
- `python tools/bypass-smoke.py`：原 21 项 + 默认信任 11 项全过（observe 级设备在默认下能跑 shell、状态和 doctor 标出默认、
  `bypass off` 后被排除并再次被拒、`bypass on` 取消排除、`default off` 后恢复拒绝、审计有 `default` 记录）。
  用例里 `level none` → `level observe` 之后要等控制端重拨（实测第 2 次、约 0.5 s 才连上），脚本里加了轮询。
- `python tools/cli-remote-smoke.py --big-mib 4`：原 39 项 + ps/kill 5 项全过（`ps --pid` 找到被控端、`ps 名字`、`kill` 拒绝杀被控端、
  `kill --pid` 杀掉 ping 树且进程真的退出）。

### 下一步

- [ ] 桌面端"设置 → 本设备"加默认信任的下拉项（现在只有命令行能开，桌面端列表能正确显示生效状态）。
- [ ] 配对时的信任预设：`pair-code --trust` 之类，让新设备一配对就按默认处理（现在默认开着时已经自动覆盖新设备，只缺提示）。
- [ ] `process_list` 的 `user` 字段（sysinfo 在 Windows 上要额外刷新，先没加）。

## 推流性能专项（2026-10-12 起，分支 `perf/stream-max`）

目标：所有平台的画面推流做到硬件编解码、原生分辨率、满刷新率、最低延迟。

### 起点：Windows 被控端是纯软件链路

改动前 Windows 的链路：Graphics Capture（`windows-capture` crate）→ 每帧**新建一张整屏 staging 纹理**并回读 CPU →
2~3 次整帧拷贝 → CPU 缩放 + BGRA→I420 → OpenH264 软编。另有硬上限：帧率 ≤60、长边 ≤3840、码率 ≤50 Mbps，只有 H.264。

实测（RTX 4070 Ti，主屏 5120×2160@100Hz，release，`capture_and_encode_the_screen`，每档一次约 4 s，桌面基本静止）：

| 请求 | 输出 | 实际帧率 | 每帧编码 |
|---|---|---:|---:|
| 1080p30 | 1920×810 | 18.2 fps | 44 ms |
| 1080p60 | 1920×810 | 13.7 fps | 49 ms |
| 4K30 | 3840×1620 | 6.8 fps | 113 ms |
| 4K60 | 3840×1620 | 6.9 fps | 135 ms |

Mac 被控端当时已是 ScreenCaptureKit + VideoToolbox H.264 硬编（零拷贝），见 `docs/stream-performance.md`。

### 现在的 Windows 链路（已实现）

```
WGC（自建 D3D11 设备：VIDEO_SUPPORT + 多线程保护，选扫描该显示器的适配器）
  → GPU 视频处理器一步完成 缩放 + BGRA→NV12（BT.709 limited）→ 4 张 NV12 纹理池
  → Media Foundation 硬件编码 MFT（NVENC/AMF/QSV，同一 D3D11 设备，异步 MFT）
  → H.264 / HEVC / AV1，CBR、低延迟、无 B 帧、无周期 IDR（只在观看者请求时出关键帧）
```

- 代码：`crates/ar-host/src/stream/capture/win.rs`（采集 + GPU 转换 + 纹理池 + NV12 回读兜底）、
  `crates/ar-host/src/stream/encode/win.rs`（MF 硬件编码器）、`encode.rs`（`Codec`、`plan`、后端选择与回退）。
- 回退顺序：GPU 采集失败 → 旧的 `windows-capture` CPU 路径；硬件编码失败 → 回读 NV12 给 OpenH264（长边降到 3840）。
- 编码器选择：观看者在请求里列出能硬解的编码（`codecs`，优先级从高到低），被控端选第一个有硬件编码器的；
  H.264 总在最后兜底。H.264 硬件上限长边 4096，超过时选 HEVC/AV1；都不行就让采集缩小（`Capture::max_long_edge`
  是共享的 `AtomicU32`，GPU 转换每帧读取）。
- 采集帧率：`SetMinUpdateInterval(1/fps)`，否则 WGC 默认按 60 Hz 节流。

实测（同一台机器，`gpu_encode_throughput`：合成的类桌面画面每帧整屏滚动 7 行，是最难编码的常见场景；不读屏幕；
release；每档 120~480 帧单次运行；"p50/p95"是 GPU 转换 + 提交 + 拿到输出的单帧耗时，不含网络和解码）：

| 尺寸 | 编码 | p50 | p95 | 背靠背吞吐 |
|---|---|---:|---:|---:|
| 5120×2160 | AV1 | 4.63 ms | 5.41 ms | 189 fps |
| 5120×2160 | HEVC | 5.79 ms | 6.53 ms | 150 fps |
| 3840×2160 | AV1 | 3.13~3.23 ms | 3.7~4.2 ms | 225~268 fps |
| 3840×2160 | HEVC | 4.4~4.5 ms | 5.2~5.4 ms | 177~197 fps |
| 3840×2160 | H.264 | 9.5~9.7 ms | 10.1~10.4 ms | 89~97 fps |
| 1920×1080 | AV1 / HEVC / H.264 | 1.8 / 2.6 / 2.9 ms | 2.5 / 3.3 / 3.9 ms | 462 / 338 / 290 fps |

首帧（编码器初始化）60~170 ms。NVENC 在 4K/5K 上 HEVC/AV1 明显快于 H.264（推测是 HEVC/AV1 能用两个 NVENC 引擎做
split-frame，H.264 不行；未查证）。所有码流都用 Edge 155 的 WebCodecs 按桌面端同一套 codec 字符串解码通过，
尺寸正确，批量解码 290~965 fps（`tools/webcodecs-decode-check.mjs`）。**AMD/Intel 显卡未测**。

### 端到端实测（真实链路）

`tools/stream-e2e.py`：本机起隔离的 host 和 controller，经真实 QUIC + controller agent 拉流（`ai-remote view`，
已扩展 `--max-long-edge/--bitrate-kbps/--codecs`，输出编码、首帧时间、帧间隔分位数）；`--desktop <exe>` 再用桌面端
调试版的 smoke 模式（屏幕外、临时 WebView2 数据目录）按画质档读取页面上的实测指标。主屏 5120×2160@100Hz，
屏幕左上角开一个小动画窗口（`--animate`），每组 6~8 s，单次运行：

| 配置 | 编码 | 尺寸 | 帧率 | 码率 | 首帧 | 帧间隔 p50/p95 |
|---|---|---|---:|---:|---:|---|
| 1080p30 | H.264 | 1920×810 | 29.8 | 1.0 Mbps | 100 ms | 33.3 / 34.0 ms |
| 1080p60（均衡） | AV1 | 1920×810 | 59.3 | 2.5 Mbps | 89 ms | 16.6 / 17.2 ms |
| 1440p120（流畅） | AV1 | 2560×1080 | 99.1 | 4.9 Mbps | 80 ms | 10.0 / 10.7 ms |
| 4K60（清晰） | AV1 | 3840×1620 | 59.5 | 9.4 Mbps | 82 ms | 16.7 / 17.4 ms |
| 原生（极致） | AV1 | 5120×2160 | 99.0 | 22.8 Mbps | 86 ms | 10.0 / 10.8 ms |
| 原生 | HEVC | 5120×2160 | 98.8 | 8.2 Mbps | 83 ms | 10.0 / 10.6 ms |

桌面端（Tauri Channel → WebView2 WebCodecs 硬解 → canvas，调试版 Rust 后端）同场景：均衡 60.0 fps、流畅 100.0、
清晰 60.1、极致 5120×2160 100.1 fps，丢帧 0、解码队列 0。这个场景码率低（1~23 Mbps），**还没压到 IPC 通道**；
高码率压测用被控端的合成图案源：`AI_REMOTE_TEST_PATTERN=5120x2160`（Windows 被控端，整屏滚动，不读屏幕；
脚本里是 `--pattern 5120x2160`）。用屏幕上的噪点窗口做压力源不可靠（窗口被遮挡或浏览器生成慢时画面不变）。

高码率压测（`--pattern 5120x2160`，整屏滚动，单次运行）：命令行端 5K AV1 99.0 fps / 197 Mbit/s、HEVC 99.0 fps /
124 Mbit/s，4K60 三种编码 59.4~59.6 fps / 43~46 Mbit/s。桌面端（调试版后端，每档约 12 s）：均衡 60.0 fps 11.9 Mbps、
流畅 100.0 fps 25.2 Mbps、清晰 60.0 fps 40.3 Mbps、**极致 5120×2160 100.0 fps 192 Mbps**，解码队列都是 0，
累计丢帧 19 / 4 / 4 / 34。**结论：Tauri Channel + WebView2 在 5K@100fps、约 190 Mbps 下仍跑满帧，暂时不是瓶颈。**
这一轮的丢帧和 AV1 码率问题已经定位并修复，见下面"码率与丢帧"。修复后同样的压测连跑 3 轮 × 4 档：
**全部 0 丢帧、解码队列 0**，均衡 59.8~60.0 fps / 11.8~12.1 Mbps、流畅 99.9~100.0 / 25.3~25.6、清晰 59.9~60.1 /
38.8~40.0、极致 5120×2160 99.2~100.0 fps / 155~157 Mbps（请求 150；指标是最后 1 秒的窗口值）。

### 码率与丢帧（2026-10-12 第二轮）

- **NVENC 的 AV1 码控不可靠**（驱动 581.57，经 Media Foundation）：目标 12 Mbps 时，原生 4K/5K 出 51/122 Mbps，
  缩放到 1080p/4K 时 11/17。把 `CODECAPI_AVEncVideoMaxQP` 设成 51（255 被拒）只是把失败从原生尺寸挪到缩放尺寸
  （99/113，设 40 时 184/321）；CBR、峰值约束 VBR、LowDelayVBR 三种码控模式结果完全一样（它不看码控模式）。
  同样条件下 H.264/HEVC 都在目标的 1.13 倍以内。**所以：**
  - 桌面端偏好改为 HEVC > AV1 > H.264（`codec.ts`）。
  - 被控端加**码率守卫**（`encode.rs` 的 `RateGuard`）：非关键帧码率连续 2 s 超过请求的 1.2 倍，就换到观看端接受的
    下一个编码（带关键帧）。回归测试 `streams_keep_to_the_requested_bitrate`：4 种尺寸 × 3 种编码 × 2 档码率，
    H.264/HEVC 不得被换走，最终码率不超过 1.15 倍。AMD/Intel 的编码器若有同类问题也会被它兜住。
- **丢帧根因**（`video.ts` 现在按原因计数并写日志，会话页指标的提示里能看到）：
  1. 积压判断太激进：排队 2 帧就丢到下一个关键帧。100 fps 下这只是 20 ms 的抖动，代价却是丢掉之后所有帧，
     外加一个几 MB 的关键帧（它又引发下一次积压）——极致档 0.5 s 内连续重置 4 次、12 s 丢 89 帧。
  2. 起流误判：WebView2 的硬件解码器启动要 100~700 ms，期间排队的帧被当成积压。
  **修复**：积压按流时间算（最老的待解帧落后最新帧多少，和帧率无关）；解码器出帧且追上之前不判（或启动 2 s 后）；
  落后 150 ms 要**持续 300 ms** 才丢（短卡顿让解码器自己追回），落后 1 s 立刻丢；关键帧到来时落后就直接用它重建。
- **兼容性修复**：旧版被控端的请求解析拒绝未知字段，新观看端带 `codecs` 会被拒（实时画面打不开）。controller agent
  遇到 "unknown field `codecs`" 时去掉该字段、在同一连接上重试（`agent.rs` 的 `without_unknown_field`）。
  已用改动前的旧版二进制当被控端实测通过（`tools/stream-e2e.py --host-cli <旧版>`）。

两处由实测驱动的修正：
- **帧率按刷新取整**：WGC 只能在刷新时出帧，"最小间隔 1/fps"会凑成整刷新：100 Hz 屏上请求 60 得 50、30 得 25；
  144 Hz 上 60 会变 48。现在 WGC 放行 1/(2fps)，在编码循环里用按时间累积的令牌桶节流（`protocol.rs` 的 `Pacer`）。
  必须在消费端节流：在采集端丢帧会丢掉"静止前的最后一次变化"，画面会停在旧帧。帧率另以显示器刷新率封顶
  （`capture::refresh_hz`），编码器按实际帧率分配码率。
- **码控改 LowDelayVBR（峰值=目标码率）**：4K60/40 Mbps 下（只算非关键帧），CBR 静止画面 19~28 Mbps、AV1 滚动画面
  超到 44 Mbps；LowDelayVBR 为 12~19 / 17~20 Mbps，编码耗时不变。（后来为了运行中改码率又改回 CBR，见 `encode/win.rs` 的 `rate_control`；画质对比见下文。）

### 协议变化（`screen/1`，向后兼容）

- 请求新增 `codecs: ["av1","hevc","h264"]`（可选，未知名字忽略）；不带时仍是 H.264，旧观看端不受影响。
- 回复新增 `codec`（起始编码）。帧头 `flags` 第 0 位是关键帧，**第 1~2 位是编码**（0 H.264、1 HEVC、2 AV1）；
  旧观看端永远收不到非 0 的编码（它们不发 `codecs`）。编码可能在关键帧处改变（硬件失败回退软件 H.264）。
- 上限：帧率 240、长边 8192、码率 500 Mbps（旧被控端仍按 60 / 3840 / 50 Mbps 截断）。
- 观看端控制行（都可选，旧被控端忽略）：`{"ping":n}` → kind-3 回流时钟；`{"bitrate_kbps":n}` 运行中改码率
  （≤ 请求值）。请求 `"cursor":"client"` → 回复带 `"cursor":"client"` 时另发 kind-4 光标位置、kind-5 光标形状。
  格式以 `protocol.rs` 的模块注释为准。

### 桌面端（Tauri，Windows + Mac 共用）

- `apps/desktop/src/lib/codec.ts`：启动时用 `VideoDecoder.isConfigSupported` 探测 4K 下能硬解的编码（HEVC > AV1 > H.264，
  原因见"码率与丢帧"），会话请求时带上。
- `video.ts`：按帧头编码选 codec 字符串；**配置时必须带 codedWidth/codedHeight**——不带的话 Chromium 的 HEVC 路径
  把显示尺寸报成 1280×720（2026-10-12 实测）。编码或尺寸变化时重建解码器。
- 延迟、自适应码率、光标叠加见下面"延迟、自适应码率、画质、光标"。
- 画质档：均衡 1080p60/12 Mbps（默认）、流畅 1440p120/25 Mbps、清晰 4K60/40 Mbps、**极致 原生/240 fps/150 Mbps**、低带宽。

### 延迟、自适应码率、画质、光标（2026-10-12 第三轮）

**延迟指标**（被控端在 `5ff8ff5`，观看端本轮）：帧头 `ts_us` 是按流时钟的采集时间；观看端发 `{"ping":n}`，被控端回
kind-3 带流时钟。桌面端 `lib/clock.ts` 取 60 s 内往返最短的一次做映射（NTP 规则，误差 ≤ 半个往返），会话页指标显示
"采集后 N 毫秒显示（±误差）"（解码出帧绘制时刻 − 采集时刻，中位数/秒）。命令行 `ai-remote view` 报"采集→到达"分位数。

**自适应码率**（`apps/desktop/src/lib/rate.ts`，被控端 `{"bitrate_kbps":n}` 在 `5ff8ff5`）：
- 排队信号：延迟比 60 s 内最低值高出的部分。每 0.5 s 评估一次，取这 0.5 s 内**最快 1/5 帧**的延迟（网络排队会推迟
  每一帧，机器忙造成的卡顿只推迟部分帧）。基线另以"半个最短 ping + 80 ms"封顶：慢链路上起流时第一次测到延迟前
  队列就已形成。
- 降：排队 > 50 ms 持续 1 s（两次评估）且没在缩短，降到"过去 1 s 实际到达速率 × 0.85"（排队 > 250 ms 用 × 0.6），
  一次最多降到 × 0.7，每秒最多一次；**队列在缩短时不再降**。到达速率不到目标一半且排队 > 250 ms 时不等第二次评估。
  排队很浅、到达又远低于目标时不动（静止画面后的突发、解码忙，不是码率的问题）。
- 升：排队 < 15 ms 持续 3 s 升 × 1.15，连续两次之后每次 × 1.3；降速后 20 s 内不超过"引发排队的码率 × 0.9"，之后
  再试探（稳定链路上大约每 20 s 一次短暂排队）。下限是请求的 1/10、至少 500 kbit/s。
- 测试工具：controller agent 的 `AI_REMOTE_TEST_LINK_KBPS`（`stream-e2e.py --link-mbps`）按给定速率读 QUIC 流，
  QUIC 窗口会像真实慢链路一样填满；桌面端 smoke 模式每秒在日志里写一行 `Live video: stats {...}`，脚本打印时间线。
- 单元测试含闭环仿真（固定链路、链路骤降再恢复、机器卡顿、起流前已排队），`tools/desktop-state.test.mjs`。

实测（本机，被控端合成图案 1920×1080 整屏滚动、HEVC、均衡档请求 12 Mbit/s，桌面端调试版；**同时有其他会话在编译，
CPU 占满**，所以有与网络无关的卡顿；每组单次运行 45 s）：

| 场景 | 控制器 | 结果 |
|---|---|---|
| 不限速 | 无（命令行，固定 12 Mbit/s） | 54 fps，到达延迟 p50 14 ms / p95 35 ms，最大帧间隔 240 ms |
| 限速 8 Mbit/s | 无（命令行） | 32 fps，到达延迟 p50 1.2 s / p95 2.0 s |
| 限速 8 Mbit/s | 旧版（每秒排队就 × 0.7） | 28 s 内从 12 降到下限 1.2 Mbit/s 并停在那里 |
| 限速 8 Mbit/s | 本版 | 起流 1.5 s 内首次降速、约 6 s 排空；15 s 之后平均接收 5.2 Mbit/s、延迟中位 18 ms、p95 216 ms（试探上探时的短暂排队）、58.3 fps |
| 不限速 | 本版 | 45 s 内一次也没降，延迟中位 13.5 ms、最大 20 ms |

限速工具实际只能过约 7 Mbit/s（降速日志里的到达速率 6.4~7.0）。另外 NVENC HEVC 在这个图案上 CBR 实际产出约为目标
的 0.75~0.85。**没测的**：真实网络（Wi-Fi、蜂窝、跨洲中继）、丢包、链路中途变化（只在仿真里测了）、4K/5K 档。

**画质客观对比**（`encoded_quality` 测试 + `tools/stream-quality.py`）：合成的类桌面画面（像素级伪文字、窗口标题栏、
带颗粒的渐变）每帧整屏滚动 7 行，原生尺寸，按 60 fps 真实节奏喂 300 帧，NVENC（RTX 4070 Ti，驱动 581.57）经 Media
Foundation；参考帧用 ffmpeg 从同一张源图按同样的滚动位置重建并转 BT.709 limited 4:2:0（与 GPU 转换器的色度插值
不同，这部分误差各行相同，只适合横向比较）。已逐帧确认对齐（错一帧 PSNR 掉到 10 dB）。每组单次运行：

| 尺寸 | 请求 | H.264（实际 / PSNR-Y / SSIM-Y） | HEVC | AV1 |
|---|---|---|---|---|
| 1920×1080 | 6 Mbit/s | 5.0 / 27.95 dB / 0.905 | 4.9 / 33.40 / 0.953 | 5.0 / 35.67 / 0.960 |
| 1920×1080 | 12 Mbit/s | 10.4 / 36.36 / 0.974 | 10.2 / 39.40 / 0.984 | 10.5 / 42.93 / 0.991 |
| 3840×2160 | 20 Mbit/s | 15.1 / 28.93 / 0.929 | 14.6 / 34.23 / 0.962 | **42.8 / 16.03 / 0.739** |
| 3840×2160 | 40 Mbit/s | 32.5 / 36.74 / 0.981 | 34.4 / 39.86 / 0.988 | 34.0 / 41.72 / 0.993 |

（"实际"是非关键帧的码率，Mbit/s。）结论：
- 同码率 AV1 > HEVC > H.264：1080p 上 HEVC 比 H.264 高 3~5.5 dB，AV1 再高 2.3~3.5 dB。
- **AV1 在 4K/20 Mbit/s 失灵**：码率超到 2 倍多，关键帧就只有 20.5 dB、之后逐帧降到 16 dB（画面糊成一片）。这和
  之前"NVENC 经 MF 的 AV1 码控不可靠"一致，所以"HEVC 优先 + 码率守卫（超 1.2 倍换编码）"的选择是对的；码率守卫
  会在 2 s 内把这种流换走。
- **CBR 与 LowDelayVBR 在这个内容上逐字节相同**（H.264/HEVC 8 对中 7 对 `cmp` 逐字节一致，第 8 对字节数相同、其 VBR 文件后来被调试运行覆盖没能 cmp；AV1 的 4 对大小相差 ≤0.6%）：整屏运动时
  预算吃满，峰值=均值的 VBR 就是 CBR。所以为了自适应码率改用 CBR，在难画面上不损失画质；代价只在简单画面上多花码率
  （见上文的 19~28 vs 12~19 Mbit/s）。
- VMAF 在这类合成内容上饱和（H.264 6 Mbit/s 28 dB 的流 VMAF 99.9），不能用来比较，脚本默认仍算，`--no-vmaf` 跳过。
- **没测的**：真实桌面内容（文字、视频、游戏）、缩放档（5K→4K/1080p）、AMD/Intel 编码器、主观画质。

```sh
AR_RC=both cargo test -p ar-host --release --lib encoded_quality -- --ignored --nocapture --test-threads=1
python tools/stream-quality.py --no-vmaf
```

**NVENC 参数（2026-10-13）**：`CODECAPI_AVEncCommonBufferSize`（VBV/HRD）和 `CODECAPI_AVEncCommonQualityVsSpeed` 在 NVENC
的 MF 编码器上**只在设置媒体类型之前生效**。之前 VBV 只在之后设，被静默忽略（`GetValue` 读回的是我们的值，但码流逐字节
不变）；而且单位按微软文档对 H.264 是**字节**，旧代码传的是"2 帧的比特数"。现在在媒体类型前后都设，值为 **0.25 s 码率的
字节数**（`encode/win.rs` 的 `vbv_bytes`）。实测（`encoded_quality`，HEVC/H.264，1080p 6/12 Mbit/s、4K 20/40 Mbit/s，
60 fps 每组 300 帧单次运行；"稳态"是第 3 秒起逐帧 PSNR-Y 均值，"起始"是第 1 秒）：

| 4K HEVC 20 Mbit/s | 关键帧 | 实际码率 | 起始 / 稳态 PSNR-Y | 之后帧大小最大/均值 |
|---|---|---|---|---|
| 改前（VBV 被忽略） | 2.37 MB | 14.6 | 29.7 / 40.2 dB | 4.7× |
| 现在（0.25 s） | 575 KB | 19.1 | 20.3 / 41.2 dB | 2.1× |

- 改前每个关键帧约是 0.6~1.0 s 的码率预算（1080p 6 Mbit/s 468~497 KB，4K 20 Mbit/s 2.0~2.4 MB，之后几帧几乎为空在"还债"）：
  链路刚好等于码率时，每次关键帧就是近 1 s 的延迟突发（以前记录的"5K 关键帧约 2 MB"就是它）。
- 现在：8 组里 7 组稳态 +0.2~2.7 dB（4K HEVC 40 Mbit/s −0.1），实际码率从目标的 73~87% 提到 92~98%，之后帧大小最大
  1.6~2.3 倍均值（改前 1.9~6.7 倍）。代价是关键帧压在缓冲内、刚出来时偏糊，由下面的"静止画面补清"解决。
- 500 ms：稳态反而低 1.0~2.7 dB；125 ms：NVENC 像是不认（关键帧又变成 0.4~3.1 MB，部分组码率超 8~9%）。保持 250 ms。
- QualityVsSpeed（0 最快，默认 33，66/100 更好）：4K HEVC 编码 2.4 / 4.3 / 11 / 22 ms，4K H.264 4.5 / 9.7 / 13.7 ms；
  稳态 PSNR 0 档比默认低 0.06~1.0 dB，66 档高 ≤0.2 dB。收益不大，保持默认（4K H.264 想上 100 fps 时可考虑 0 档）。
- 端到端（`--pattern 5120x2160`，命令行端，单次）：各档帧率不变（59.3~59.6 / 99.0~99.1 fps），采集→到达 p50 4.5~16 ms。
  回归 `streams_keep_to_the_requested_bitrate`、`bitrate_changes_take_effect_mid_stream`、
  `hardware_encoders_open_and_close_repeatedly` 都通过。
- **静止画面补清**（`protocol.rs` 的 `Refine`）：关键帧本身在这张合成图上只有 14~15 dB，**加大关键帧没用**（关键帧时
  码率 ×4、关键帧 1.5 MB，第 0 帧也只从 14.5 到 15.7 dB，第 30/60 帧和不加一样）；画面在动时后续 P 帧会自己补清，
  但桌面静止时采集不出新帧，糊的关键帧（或滚动停下前最后一张运动中的帧）会一直停在屏幕上。现在：关键帧或较大的
  新帧（≥ 每帧码率份额的 10%）之后，没有新帧时按帧率把最后一张图再编码，直到连续 0.25 s 输出都小于份额的 10%
  （关键帧后头 0.4 s 不计：码率控制在用空帧还关键帧的债），最多 5 s。光标闪烁这类小新帧不触发。
  实测（单次，命令行端存码流逐帧打分；桌面端 smoke 确认 0 丢帧）：
  - 静止图案（`--pattern 1920x1080,still`，均衡档 HEVC 12 Mbit/s）：15.1 dB → 约 1.6 s 后 47.2 dB，随后停发；补清期间
    1.8 MB（平均约 7.7 Mbit/s，低于请求）。4K 清晰档 40 Mbit/s：14.5 → 47.1 dB，6.7 MB。改动前屏幕会一直停在第 0 帧。
  - 滚动 2 s 后停下（`--pattern 1920x1080,stop=2`）：停下那一帧 41.9 dB，20 帧后 43.5 dB 收敛，之后每帧 1.1 KB 后停发。
  - 持续滚动时不介入：5K 图案各档帧率不变。
  - **没测的**：真实桌面内容（文字界面的关键帧本来就比这张图清楚，补清应更快）、补清期间网络正好变慢时与自适应码率的
    交互。
- **低码率时降分辨率：不做。** 同一 1080p/4K 源在 1.5~10 Mbit/s 下缩到 0.5~0.75 倍编码再放大回来，PSNR-Y 只有 12 dB
  （原尺寸 21.8~33.5 dB）：1 像素的文字笔画缩放后直接变成灰色。桌面内容（文字、界面）低码率时保持原尺寸只降码率；
  视频、游戏类内容可能不同，未测。测量用 `AR_VARIANTS=edge=N`。

**光标本地绘制**（`stream/pointer.rs`、`protocol.rs` 的 `PointerFeed`，桌面端 `lib/pointer.ts`）：
- 观看端请求带 `"cursor":"client"`；Windows 被控端走 WGC 时 `SetIsCursorCaptureEnabled(false)`（DDA 和测试图案本来就不画），
  回复 `"cursor":"client"`，然后按帧率（30~120 Hz）查 `GetCursorInfo`，变了才发 kind-4 位置（JSON，相对被串流显示器，
  单位同回复的 `screen`）；新形状先发 kind-5（`GetIconInfo` + `GetDIBits` 转直通 alpha 的 BGRA；单色光标的反色像素画成
  黑色加白边）。CPU 回退路径（`windows-capture`）和 macOS 仍把光标画进画面、不回 `cursor`，观看端照旧。
- 兼容：旧被控端拒绝未知字段 → controller agent 去掉 `cursor` 重试（`without_unknown_field`）；只有请求了的观看端才会
  收到 kind 4/5，命令行默认不请求（`ai-remote view --pointer` 请求并计数），iOS 未接入。
- 桌面端：叠加一个 `pointer-events: none` 的 canvas，位置变化只改 CSS transform（不经过 React），形状按"CSS 像素 / 屏幕像素"
  缩放；形状 0 或没收到形状时画一个默认箭头。
- 实测（本机，单次）：请求后收到 1 个位置 + 1 个形状（48×48，9.3 KB）；桌面端 smoke 收到形状、60 fps、无页面错误。
  **没测的**：移动鼠标时省下多少编码帧/码率（要移动这台电脑上正在用的鼠标，没做）、多显示器 / 不同 DPI 下的位置、
  动画光标（只取第一帧）、手动操控时本地光标和远端光标同时显示的观感。

### 踩过的坑（都已修，别再踩）

1. **MF 硬件编码器必须 `IMFActivate::ShutdownObject`**。只 Release 激活对象会泄漏 MFT 和它的 NVENC 会话；同进程
   下一个编码器初始化会卡 4.7 s ~ 30 s（实测），去掉 ShutdownObject 的对照组 600 s 被强杀。
   回归测试：`hardware_encoders_open_and_close_repeatedly`（12 轮，现在每轮首帧 61~167 ms）。切画质/重连会命中这里。
2. **NVENC 忽略 `CODECAPI_AVEncMPVGOPSize`**，默认每秒一个 IDR（5K 下约 2 MB，100 Mbps 要传 18 ms，每秒一卡）。
   要在输出媒体类型上设 `MF_MT_MAX_KEYFRAME_SPACING`。
3. OpenH264 的解码器解不了 NVENC 的 4K H.264（High、level 6.0），但 WebCodecs 能解；验证码流用 WebCodecs。
4. `windows-capture` 的 `Frame::buffer()` 每帧分配整屏 staging 纹理，且它建设备不带 VIDEO_SUPPORT，所以自己写了 WGC。
5. 闭包捕获 `Shared(x).0` 会按字段捕获（edition 2021 disjoint capture），绕过 `unsafe impl Send` 包装；用方法访问。
6. 测码率要按真实帧率节奏喂帧：背靠背喂帧时编码器按时间戳看到的是 2 倍帧率，"平均帧大小 × 帧率"会掩盖超标。
   而且要测**缩放后的尺寸**：AV1 的问题在原生尺寸和缩放尺寸上表现相反。
7. 单个"修复"要在所有画质档上复测：MaxQP=51 修好了原生 4K/5K，却让 1080p/4K 缩放档的 AV1 从 12 Mbps 涨到 105。

### 下一步（按收益排序）

**Windows（这台机器能测）**
- [x] 真实端到端（见上表）：5K2K@100Hz 原生推流，命令行 99 fps、桌面端 100 fps 丢帧 0。
- [x] 高码率下 Tauri Channel：5K@100fps / 192 Mbps 跑满帧（见上）。原理备忘：每帧 >1 KB 时 Tauri 2.12 先 `eval`
      一段 JS，页面再走一次 `ipc://` fetch（`tauri/src/ipc/channel.rs`）；更高帧率/码率若出现排队，再换本地 WebSocket
      （随机令牌、只听 127.0.0.1）或 WebView2 SharedBuffer。
- [x] 桌面端丢帧：定位并修复（见"码率与丢帧"），压测 12 次 0 丢帧。
- [x] AV1 码率超标：HEVC 优先 + 码率守卫（见"码率与丢帧"）。直接用 NVENC SDK 的 AV1 也许码控正常，未试。
- [x] 画质客观对比：见"延迟、自适应码率、画质、光标"。真实桌面内容和缩放档还没测。
- [x] 端到端延迟指标：会话页显示"采集后 N 毫秒显示（±误差）"。
- [x] 自适应码率（只调码率，不调分辨率/帧率；降分辨率在桌面内容上实测更差，见"NVENC 参数"末条）。下一步：真实网络实测。
- [x] 调 NVENC 参数：VBV 之前没生效，现在 0.25 s；QualityVsSpeed 保持默认（见"NVENC 参数"）。文字清晰度（4:4:4）解码端普遍不支持，暂不做。
- [x] 关键帧起始偏糊（VBV 生效的代价）：加大关键帧无效，改为静止画面补清（见"NVENC 参数"）。
- [x] 光标改为客户端绘制（Windows 被控端 + 桌面端）。2026-10-20 补了 macOS 被控端和 iOS 观看端（见"Mac 被控端补全"一节）。
      待做：移动鼠标时的收益实测。

**Mac 被控端** — 2026-10-20 全部做完，见下面"Mac 被控端补全"一节。
- [x] `capture::refresh_hz`：CGDisplayCopyDisplayMode / CGDisplayModeGetRefreshRate。
- [x] `AI_REMOTE_TEST_PATTERN`：共享的 `capture::pattern` 模块，Mac 上写进 IOSurface 池。
- [x] VideoToolbox HEVC，H.264 改 High，去掉周期 IDR，低延迟码控，在线改码率。
- [x] 帧率上限 240、长边上限 8192（按编码器再限：H.264 4096）。
- [x] `tools/webcodecs-decode-check.mjs` 在 Mac 上用 Chrome 跑过：HEVC/H.264 都解。

**iPhone 主控端** — `apps/ios/Sources/H264Decoder.swift`、`crates/ar-mobile`
- [x] 请求里带 `codecs: ["hevc","h264"]` 和 `cursor: "client"`，解码器按帧头编码位走 HEVC，画面上叠加被控端光标
      （2026-10-20，只过了 macOS 上的 `swiftc -typecheck`，UIKit 部分靠 iOS CI）。

**传输**
- [ ] QUIC 接收窗口：高码率 + 高 RTT 时单流窗口可能不够（`ar-net` 拨号可传 `QuicTransportConfig`）。先测再改。

### 复现命令

```sh
# Windows 硬件编码吞吐（合成画面，不读屏幕）；码流写到 target/stream-bench/
cargo test -p ar-host --release --lib gpu_encode_throughput -- --ignored --nocapture
AR_SIZES=5120x2160 AR_CODECS=hevc,av1 AR_KBPS=120000 cargo test -p ar-host --release --lib gpu_encode_throughput -- --ignored --nocapture
# 编码器反复创建（会话泄漏回归）
cargo test -p ar-host --release --lib hardware_encoders_open_and_close_repeatedly -- --ignored --nocapture
# 用 WebCodecs 解上面的码流（默认 Edge，可传 Chrome 路径）
node tools/webcodecs-decode-check.mjs
# 端到端：隔离 host/controller + 真实 QUIC；--desktop 再测桌面端（先 cd apps/desktop && npx tauri build --debug --no-bundle）
python tools/stream-e2e.py --animate --desktop target/debug/ai-remote-desktop.exe
python tools/stream-e2e.py --pattern 5120x2160 --desktop target/debug/ai-remote-desktop.exe   # 高码率、可重复
# 自适应码率：限速 8 Mbit/s，桌面端每秒一行指标（日志里的 "Live video: stats"）
python tools/stream-e2e.py --pattern 1920x1080 --link-mbps 8 --skip-cli --qualities balanced --seconds 45 --desktop target/debug/ai-remote-desktop.exe
# 画质：PSNR/SSIM（VMAF 在合成内容上饱和）
AR_RC=both cargo test -p ar-host --release --lib encoded_quality -- --ignored --nocapture --test-threads=1 && python tools/stream-quality.py --no-vmaf
# 编码器调参：AR_VARIANTS="early=0;default;vbvms=125;qvs=0;scroll=0;boost=2,rekey=150"（见 encoded_quality 的注释）
# 静止画面补清：被控端只出一帧（或滚动 N 秒后停），存码流后逐帧打分
python tools/stream-e2e.py --pattern 1920x1080,still --only balanced --save --out target/e2e-still
# 光标单独发送：命令行计数 kind 4/5
python tools/stream-e2e.py --pattern 1920x1080 --pointer --only balanced
# 旧的真实屏幕采集 + 编码（会读当前屏幕，只在内存里）
cargo test -p ar-host --release --lib capture_and_encode_the_screen -- --ignored --nocapture
```

## 锁屏 / 登录界面下的远程访问（2026-10 起）

背景与方案见 `CLAUDE.md` 的"当前专项"（RustDesk 式：服务 + 跟随输入桌面 + DXGI 采集）。按模块推进，每步编译、测试、本机验证。

### 已做：DXGI Desktop Duplication 采集后端（`stream/capture/dda.rs`，2026-10-12）

- 和 WGC 共用 `win.rs` 的 `Gpu`（同一个 D3D11 设备）和 `Converter`（GPU 缩放 + BGRA→NV12），产出同样的
  `Raw { gpu }` 放进 `Slot`，后面的硬件编码链路不变。`win.rs` 只把 `Gpu::for_monitor`、`Converter` 及其
  `new`/`convert` 改成 `pub(super)`。
- 采集线程：`OpenInputDesktop` + `SetThreadDesktop` 跟随输入桌面（RustDesk `windows.cc` 的
  `selectInputDesktop`）；`AcquireNextFrame` 出错（`DXGI_ERROR_ACCESS_LOST`：桌面切换、改分辨率、独占全屏）或
  `ReleaseFrame` 失败就重建 duplication，失败时每 500 ms 重试一次，只记一条警告；画面静止时每 100 ms 比对一次
  线程桌面和输入桌面的名字，不一致也重建。首次 duplication 失败时 `start` 直接返回错误。
- **不是默认路径**：只有 `AI_REMOTE_CAPTURE=dda` 时 `capture::start` 才用它，失败就回退 WGC。
- TODO：光标没合成进画面（要用 `DXGI_OUTDUPL_FRAME_INFO` 和 `GetFramePointerShape` 画上去）；旋转屏的画面是
  没转正的（只记一条警告）；HDR 桌面走 `DuplicateOutput`，拿到的是被截断的 BGRA8。
- **还没验证锁屏/UAC**：现在被控端以登录用户身份运行，锁屏后 `OpenInputDesktop(Winlogon)` 会被拒，DDA 只会
  反复重试，等解锁后恢复。真正采到安全桌面要等服务化（SYSTEM 身份、在会话里启动）那一步。本机解锁/锁屏切换的
  恢复也还没实测。

**坑：不能让 `AcquireNextFrame` 自己等。** 第一版用 `AcquireNextFrame(100 ms)` 阻塞等新帧，同设备上的 MF 编码器
被拖慢 4~6 倍。对照实验（5120×2160 HEVC，同一张 NV12 纹理反复编码，每 10 ms 一帧，各 120 帧单次运行）：
DDA 线程停掉时编码 p50 6.4 ms；DDA 线程在跑、阻塞等待时 25~49 ms；改成不等待（timeout 0）后 5.3 ms。
推测是设备开了多线程保护，`AcquireNextFrame` 等待期间一直持有设备锁（未查证）。现在的做法是先
`IDXGIOutput::WaitForVBlank` 等到刷新，再 `AcquireNextFrame(0)`；`WaitForVBlank` 失败时睡 1 ms 兜底。
RustDesk 不受影响，因为它的采集器单线程、独占自己的设备。

**实测**：`dda_encode_throughput`，RTX 4070 Ti / 驱动 581.57，主屏 5120×2160@100Hz，release。采的是真实屏幕，
测试在左上角开一个 96×96 的窗口约每 4 ms 变一次色，好让屏幕持续刷新（DDA 在静止画面上不出帧），屏幕其余部分
基本静止。所以这是"小区域变化"的常见场景，**和 `gpu_encode_throughput` 的整屏滚动不可比**，码率也不能用来
比较。各编码每轮 120 帧，跑了 3 轮；首帧（编码器初始化）不计。"采集→编码完成"从 `AcquireNextFrame` 返回算到
`encode_raw` 返回，包括 GPU 转换、经 Slot 交接、编码器提交和取回输出：

| 输出尺寸 | 编码 | 采集→编码完成 p50 / p95 | 其中 encode_raw p50 / p95 | 帧率 |
|---|---|---|---|---:|
| 5120×2160 | HEVC | 5.39~5.45 / 5.66~5.72 ms | 5.18~5.24 / 5.46~5.47 ms | 99.7~100.6 |
| 5120×2160 | AV1 | 4.29~4.36 / 4.43~4.57 ms | 4.08~4.12 / 4.22~4.31 ms | 99.9~100.4 |
| 4096×1728（H.264 上限） | H.264 | 5.26~5.29 / 5.45~5.51 ms | 5.05~5.07 / 5.23~5.26 ms | 99.7~100.1 |
| 1920×810（`AR_EDGE=1920`，1 轮） | HEVC / AV1 / H.264 | 2.08 / 1.46 / 1.61 ms（p50） | 1.87 / 1.22 / 1.39 ms | 100~101 |

帧率被 100 Hz 刷新率封顶（DDA 每次刷新最多出一帧）。单帧最大值偶尔有 7~18 ms（每轮 120 帧里有一两帧），
原因没查。DDA 线程本身的 CPU 占用没测。

```sh
cargo test -p ar-host --release --lib dda_encode_throughput -- --ignored --nocapture
AR_EDGE=1920 AR_CODECS=hevc cargo test -p ar-host --release --lib dda_encode_throughput -- --ignored --nocapture
# 端到端试用：被控端设 AI_REMOTE_CAPTURE=dda 后照常连接
```

### 桌面切换恢复验证工具（`dda_desktop_switch_recovery`，2026-10-13，分支 `feat/lock-screen`）

把"桌面切换后 DDA 能不能干净恢复"做成一个可手动跑的测量测试，放在 `stream/capture/dda.rs` 的 `bench` 模块，
**只加测试、不改采集逻辑**。它采主屏 `AR_SECS` 秒并把帧从 slot 抽走，和吞吐测试共用那个每 ~4 ms 变色的 96×96
窗口让屏幕一直在刷新——这样"长时间没帧"只会是重建，不会是静止画面。度量方式（都从观看端能观察到的角度）：

- **卡顿时长**：相邻两帧间隔超过 `AR_GAP_MS`（默认 250 ms）就打一行 `t+<秒> frozen <ms>`。重建期间 slot 不出帧，
  所以这个间隔就是观看端看到的冻结时长。模块自身的 `Desktop Duplication restarted` / `capture thread switched
  desktop` / `... unavailable; retrying` 日志（测试开了 tracing）会穿插进来，和每次卡顿对上。
- **黑帧**：每次卡顿后的第一帧、以及平时每 ~0.5 s，用 `win::read_nv12` 把 Y 平面读回 CPU 算均值；均值 ≤ 20
  当作"接近黑"（视频黑 Y≈16，纯黑 0）。恢复后的第一帧是正常还是黑，就靠这个判出来——没有真实观看端显示画面也能看到。
- 结束打印汇总：总帧数 / 平均帧率 / 卡顿次数 / 最长卡顿 / 接近黑的采样数；capture 自己提前关了会 `assert` 失败。

一切都在本机、不出网。`AR_READBACK=0` 可只测间隔、不读回。跑法（跑起来后照提示手动操作）：

```sh
cargo test -p ar-host --release --lib dda_desktop_switch_recovery -- --ignored --nocapture
# 期间手动各做至少两次：Win+L 锁屏等 ~2 s 再解锁；改分辨率再改回；进独占全屏游戏再退出。
```

**实测结果：未测。** 工具编译通过（`cargo test --lib --no-run` 过），但锁屏→解锁要输密码、改分辨率是系统显示设置、
开游戏都得人工操作，这些不由 agent 代劳，留给维护者跑一轮把"重建耗时 / 漏帧 / 黑帧"的实测数填进来。注意当前被控端以
登录用户身份运行，锁屏后打不开 `Winlogon` 桌面，预期表现是"锁屏起到解锁为止一直冻结、解锁后恢复"（安全桌面采不到要等
服务化那步）；这一轮要测的是**解锁后、改分辨率、全屏游戏进出**这些在普通桌面上就会发生的切换，恢复干不干净、要多久。

### 设计决定：服务化边界选 A（2026-10-13，用户确认）

被控端服务化时整个主进程以 **SYSTEM 身份在会话里启动**（RustDesk 式的 A 方案，而非只拉一个采集/注入助手的 B）。
**后果（要在落地时写清）**：服务化后 `shell` / `jobs` / `files` 等工具都会以 SYSTEM 执行，一个 admin 级（或 bypass）
控制端的"最坏情况"从"等于登录用户"升到"等于 SYSTEM"。闸门 / 级别 / 策略 / 审批 / 审计的代码不变，但一个级别授予的
**实际权限被放大**了。这一点必须在 `install/windows.rs` 的 `--dry-run` 安装计划和用户文档里显式写出，让维护者在管理员
终端执行服务注册前就看到；锁屏时审批弹窗仍不显示，需要审批的操作只能由审批设备批。本轮先做纯用户态、不碰系统权限的
那一块（输入跟随输入桌面）。

### 注入跟随输入桌面（`platform::follow_input_desktop`，2026-10-13）

- 做了什么：`platform/windows.rs` 加 `follow_input_desktop()`，注入前把**注入线程**切到当前输入桌面
  （`OpenInputDesktop` + `GetThreadDesktop` 比名字，不同才 `SetThreadDesktop`）。`EnigoBackend::new()`（Windows）
  在建 enigo 前调它。注入跑在 `spawn_blocking` 线程、非 macOS 每次调用都新建 backend，所以**每次注入都重设一次**。
  镜像采集侧 `stream/capture/dda.rs` 的 `InputDesktop` 和 RustDesk `input_service` 的 `try_change_desktop`。
- 为什么现在（用户态）就有用：`SendInput` / `SetCursorPos` 发到**调用线程所在桌面**的输入队列；桌面切换（解锁、UAC、
  应用自建桌面、屏保进出）之后线程可能停在旧桌面句柄上，输入落到空处。切桌面修这个。
- 局限（说清）：用户态进程打不开 `Winlogon` 安全桌面，所以**锁屏本身的注入仍然要等服务化（SYSTEM）那步**才能恢复；
  这一步是服务化的前置铺垫——服务化后它才让注入跟上锁屏 / UAC 桌面。
- 不泄漏 + best-effort：`thread_local` 持已切到的桌面句柄，切换成功后才关旧句柄（`CloseDesktop` 拒绝关线程仍在用的桌面），
  所以稳态下每个 blocking 池线程最多一个句柄，不是每次调用泄漏一个。打不开输入桌面或 `SetThreadDesktop` 失败就留在原桌面、
  什么都不改——**已解锁路径零回退**。
- 坑 / 取舍：注入线程是 tokio blocking 池线程，可能被别的 blocking 活（截图等）复用，`SetThreadDesktop` 要求线程无窗口 /
  hook，失败即 no-op。和 `dda.rs` 的 `InputDesktop` 有约 20 行重复（`OpenInputDesktop`/`SetThreadDesktop`/`desktop_name`），
  本轮不动采集逻辑没合并，列 TODO 以后统一。切桌面只在 `new()` 时（= 每个工具调用一次）做一次；长 plan 中途切桌面
  （例如打字途中弹 UAC）未处理，RustDesk 是每次注入前都切——够用，列为后续细化。
- 实测：`cargo build` / `cargo clippy --all-targets -D warnings` / `cargo test -p ar-host` 全过（222 passed, 9 ignored）；
  新加的 `follow_input_desktop_is_safe_and_idempotent` 冒烟测试过（只验不 panic、幂等、线程仍停在可读桌面，不是功能验证）。
  **真正的"解锁后注入恢复"未手动实测**——要本机：发一次注入 → Win+L 锁屏再解锁 → 再发注入，开 tracing（debug 级）看
  `input thread switched desktop` 日志、确认注入恢复。留给能本机操作的维护者跑一轮填回。

### 服务化（A 方案）落地：`host install --service`（2026-10-13，分支 `feat/lock-screen`）

把上一节的设计决定做成代码。改动按模块：

- **服务进程** `ar-host/src/service.rs`（仅 Windows）：裸 `windows` crate 实现 SCM 入口（`StartServiceCtrlDispatcherW` /
  `RegisterServiceCtrlHandlerExW` / `SetServiceStatus`，没有引入 `windows-service` crate），服务进程本身跑在 session 0、
  不碰桌面，只负责把 `ai-remote-hostw --supervised --owner-sid <sid>` 以 **SYSTEM 身份在活动控制台会话**里拉起来：
  自己的令牌 `DuplicateTokenEx` → `SetTokenInformation(TokenSessionId)` → `CreateEnvironmentBlock` →
  `CreateProcessAsUserW`（`lpDesktop = winsta0\default`，先挂起、进 kill-on-close + breakaway 的 job 再恢复）。
  每秒轮询 `WTSGetActiveConsoleSessionId`：会话变了（注销 / 切用户 / 重启后的登录界面）就杀掉重拉；子进程异常退出按
  `supervise::Backoff`（和 hostw 守护进程共用，抽成了 `ar-host/src/supervise.rs`）退避重启；退出码 3（配置目录被终端里
  的被控端占着）每 30 s 重试；**退出码 0（`stop-host`）服务跟着停**，和守护进程"正常退出结束两者"语义一致。
- **Ctrl+Alt+Del**：`SendSAS` 只在服务进程里可靠，所以服务创建一个自动复位的命名事件 `Global\ai-remote-sas-<id>`
  （SYSTEM 建的，默认 DACL 只有 SYSTEM / 管理员能 signal），会话内被控端按 `ctrl+alt+del` 时不再按键而是 signal 它
  （`inject.rs` 新增 `Op::SecureAttention(event)`，`keys::is_secure_attention` 识别 ctrl+alt+del / ctrl+alt+end），服务等到
  事件就 `SendSAS(false)`。事件不存在（不是服务模式）时该组合键仍按原来的理由拒绝；必须单独发（不能混在序列里）。
- **桌面可达性语义**（`platform/mod.rs` / `windows.rs`）：`SessionState` 的 `desktop_usable` 从"输入桌面叫 Default"改成
  "能打开输入桌面"，新增 `secure_desktop`（能打开但不是 Default）。用户态被控端锁屏后 `OpenInputDesktop` 失败 → 和以前一样
  不可用（错误文案多了一句"装成服务可达"）；SYSTEM 被控端能打开 `Winlogon` → 可用 + secure。`prompt_blocker()`（审批弹窗）
  在 secure 时仍返回原因；新增 `desktop_blocker()` 给推流（`stream/protocol.rs` 连接与循环中的检查都换成它）。doctor 的
  `desktop` 项：可达的安全桌面是 **warn** 而不是 fail。截图工具（`tools/screen.rs`）在 xcap 采集前也调
  `follow_input_desktop()`（GDI 读的是线程所在桌面）。
- **自动选 DDA**（`stream/capture.rs::want_dda`）：`runs_as_system()`（令牌是 `S-1-5-18`）或当前就在安全桌面上时默认走
  Desktop Duplication；`AI_REMOTE_CAPTURE=dda|wgc` 仍可强制。用户态、已解锁时仍是 WGC，行为不变。
- **控制管道**：vendored mcpmesh `local-api` 加 `bind_local_shared(path, sids)`（DACL 多加一条 ACE，只接受形如 `S-1-…` 的纯 SID，
  防 SDDL 注入；unix 下是 `bind_local` 别名），`ar-net::ipc::Listener::bind_shared`、`ar_host::Claim::acquire_shared`
  串起来；hostw 新增隐藏参数 `--service` / `--owner-sid`。已登记到 `vendor/mcpmesh/PROVENANCE.json`。
- **安装计划**（`install/windows.rs`）：`install_plan(profile, scope, service)`。服务计划的 notes 里显式写了"SYSTEM 后果"、
  管道允许的账户、锁屏时审批只能靠审批设备；步骤：删同配置目录的登录任务（若有）→ `sc create|config`（存在则 config，
  `binPath=` 用 `quote_arg` 包整条命令行）→ `sc description` → `sc failure` → `SoftwareSASGeneration` 不是 1/3 时
  `reg add … /d 1`，原值写进 `host/sas-policy-before.txt`，卸载时按它恢复或删除 → 原有防火墙步骤。全部标 admin；
  标准用户执行直接拒绝（`hostops::admin_steps_allowed`）。`status` 优先识别服务（`sc qc` 解析 `BINARY_PATH_NAME`，
  `running` 字段来自 `sc query`），`start_action` 有服务时用 `sc start`，卸载时顺带 `sc stop/delete`，防火墙规则保留判断
  也把其他配置目录的服务算进去。macOS 传 `--service` 直接报错。
- 文档：`docs/operations.md` / `.zh-CN.md` 第 3.2 节新增"作为服务运行"小节，7.3 / 8 节的表格补了服务模式下的新文案。

**验证（实测 / 未测分清）：**

- 实测：`cargo build`、`cargo clippy --workspace --all-targets -D warnings`、`cargo fmt`、前端 node 测试 48 过、`tsc` 过；
  `cargo test --workspace --locked` 全过（ar-host 229 passed / 9 ignored，vendored mcpmesh local-api 75 passed）。单元测试覆盖：SDDL 多 ACE 与 SID 校验（mcpmesh）、服务名 / 事件名 /
  事件不存在时的行为、非 SCM 启动 `run()` 干净失败、`sc qc` / `sc query` / `reg query` 输出解析、`binPath` 引号、
  ctrl+alt+del 在有事件时变成单步 `SecureAttention`、doctor 对可达安全桌面给 warn。
- 实测：本机用临时配置目录跑 `host install --service --dry-run`、`host uninstall --dry-run`、`host status`、`doctor`，
  计划输出和预期一致（见 `docs/operations.md` 3.2 节描述），没有改系统。
- **未测（需要管理员终端，agent 不执行系统改动）**：真正 `sc create` 注册、服务以 SYSTEM 在会话里拉起被控端、锁屏 /
  UAC / 重启登录界面下的推流与注入、`SendSAS` 实际生效、`stop-host` 让服务一起停、卸载恢复策略。这一整条链路要维护者
  在管理员终端执行 `ai-remote host install --service` 后本机走一遍：锁屏 → 从控制端看画面 / 输密码解锁 → 弹 UAC 看是否
  能操作 → `ctrl+alt+del` → 重启后登录界面。`CreateProcessAsUserW` 这一段的参数照 RustDesk `LaunchProcessWin` 写的，
  没在本机以服务身份跑过，第一次跑失败的话看 `host/host.log` 里服务的 `cannot start the host` 一行。
- 已知没做：macOS 的 LoginWindow（`LimitLoadToSessionType`）——用户级 LaunchAgent 装不进 LoginWindow 会话，要改成
  `/Library/LaunchAgents` 的系统级安装，本机没有 Mac 编译验证，没动。DDA 光标合成、`dda_desktop_switch_recovery` 实测、
  `follow_input_desktop` 手动验证同上一节，仍未做。

### 下一步

- [x] **服务模式真机实跑**（2026-10-20/21，Windows 开发机，经 zremote 提权会话；详见本节末"真机结果"）：安装、服务起
      SYSTEM 被控端进 Console 会话、跨网段直连、安全桌面与真实锁屏上的截图 / 推流 / doctor / `ctrl+alt+del`（SendSAS）/
      **键鼠注入**（修了一个权限 bug 后）、锁屏时服务跟随会话切换、重启后服务自启、注销后的登录界面、`stop-host`
      连带停服务再 `sc start` 拉起、`host uninstall` 恢复（服务删除、SAS 策略回 0、防火墙规则清空），全部通过。原条目：
      `ai-remote host install --service`，锁屏 / UAC / 重启登录界面 /
      ctrl+alt+del / stop-host / uninstall 各走一遍，把结果填回来；失败先看 `host/host.log`。
- [x] `follow_input_desktop`：真机上切桌面本身成功，但注入仍被拒，根因是打开输入桌面的句柄缺 `DESKTOP_JOURNALPLAYBACK`
      （d42a8b4 改为 GENERIC_ALL，RustDesk 同），修后锁屏上键鼠都通（见"真机结果"）。
- [x] macOS 锁屏 / 登录界面：锁屏在 `platform/macos.rs` 里按"可达的安全桌面"处理；登录界面靠
      `host install --service` 的系统级 LaunchAgent（`LimitLoadToSessionType = [LoginWindow, Aqua]`）。代码 2026-10-20
      落地，**真机锁屏 / 注销 / 重启验证未做**，见"Mac 被控端补全"一节。
- [ ] 跑 `dda_desktop_switch_recovery`，把解锁 / 改分辨率 / 全屏游戏进出三种切换的重建耗时、漏帧、黑帧实测填回上面。
- [ ] DDA 下把光标画进画面（`DXGI_OUTDUPL_FRAME_INFO` / `GetFramePointerShape`）。现状辨析：请求 `cursor:"client"`
      的观看端在 DDA 下**已经能看到光标**（光标走带外单独发，`protocol.rs` 的 `PointerFeed`），桌面端 `Session.tsx`
      总是带 `onPointer`、总会请求，所以**一级客户端在 DDA 下不缺光标**。真正缺的是"不请求 cursor:client、只认画面里光标"
      的观看端——这类观看端不消费带外光标，唯一的办法是把光标合成进画面。做法建议：复用 `stream/pointer.rs` 的
      `position()`（GetCursorInfo）+ `shape()`（GetIconInfo→直 alpha BGRA，已处理各种光标类型，省掉 DXGI 的
      MONOCHROME/MASKED 分支），在 `dda.rs` 把 DDA 帧先拷到一张可当 RTV 的纹理、用一个 alpha 混合的贴图 quad（VS+PS+
      BlendState）把光标画上去，再交给 `Converter::convert`。约 200+ 行 D3D11+HLSL，落在 5 ms 的热路径上，要测对热路径的
      影响、并肉眼确认各种光标画得对——**没做**：这段 GPU 合成没法 headless 验证，不适合 agent 盲写上热路径，留给能看屏验证的人。
- [x] 服务化以后在锁屏和 UAC 场景下自动选 DDA：已做（`capture::want_dda`），环境变量保留为强制开关。

### 真机结果（2026-10-20，Windows 11 开发机，PR #11 分支 + bd9cd83）

通过 zremote 的提权会话在开发机上执行，Mac 作控制端（Mac 10.1.120.x，开发机 10.1.77.x，不同网段）。

- `host install --service --dry-run` 与文档 3.2 节一致；真装一次成功：服务 pid 在 session 0，
  `ai-remote-hostw` 以 `NT AUTHORITY\SYSTEM` 跑在 Console 会话（session 1），`host status` / `doctor` 全绿
  （relay 警告：公司网无外网）。`CreateProcessAsUserW` 这段第一次跑就对。
- Mac 配对后 `hosts` 报 `path: direct`（跨网段直连，无中继）；`shot`、`info`、`doctor --host` 正常。
- `keyboard ctrl+alt+del` 走 SendSAS 生效：`info` 变 `input_desktop: Winlogon, secure_desktop: true`，`doctor` 的
  `desktop` / `approval` 按设计变 warn，**安全桌面截图正常**（DDA 在 SYSTEM 下采到 SAS 菜单）。
- **安全桌面上的注入失败**：`mouse move` 报 `拒绝访问 (os error 5)`（SetCursorPos），`keyboard enter/escape` 报
  `not all input events were sent (UIPI)`。改成在全新 OS 线程上注入（bd9cd83）后仍失败，host.log 没有
  `cannot switch` 的 warn。对照实验：用被控端的 `shell` 工具（同一个 SYSTEM 令牌、同一 Console 会话）跑一段
  P/Invoke 脚本，在新线程里 OpenInputDesktop(GENERIC_ALL 或本项目的标志) → SetThreadDesktop → SetCursorPos →
  SendInput **全部成功**，桌面名 Winlogon；以用户身份跑则 OpenInputDesktop 就是 err 5。所以系统层没问题，
  差异在被控端进程内部，未定位。2026-10-21 补一条（f850318 把切桌面日志提到 info）：被控端日志里确实有
  `input thread switched desktop from="Default" to="Winlogon"`，紧接着 SendInput 仍报 os error 5——
  **线程已经在 Winlogon 上，注入照样被拒**。那么差异不在线程所在桌面，而在进程本身（对照脚本是被控端的子进程，
  却能成功）：候选是被控端进程的 integrity level / 令牌属性（服务用 DuplicateTokenEx + TokenSessionId 造的令牌
  和子进程继承的是否完全一致）、UIPI 相关的进程属性、或 enigo 事件里的 dwExtraInfo。
  **2026-10-21 定位并修复**：在失败点打印上下文（f21cdc0，`platform::input_context`）——线程桌面 Winlogon、输入桌面
  Winlogon、本进程和前台窗口都是 System 完整性，全部对称；但在**同一个线程**上紧接着用 `GENERIC_ALL` 重新 OpenInputDesktop
  + SetThreadDesktop 后，SetCursorPos 和 SendInput 立刻成功。差异只在桌面句柄的访问权限：原来沿用采集侧的标志集
  （没有 `DESKTOP_JOURNALPLAYBACK`），SendInput 对安全桌面要求这个权限；改成 `GENERIC_ALL`（d42a8b4，RustDesk 同）后，
  SAS 菜单和真实锁屏上 `mouse move`、`keyboard escape` 都成功，Esc 真的关掉了 SAS 菜单（`info` 回到 Default）。
  用户会话的 Default 桌面上原标志集够用，所以之前从没暴露。`input_context` 的探针留着：只在注入被拒时跑，会发一个
  相对 (0,0) 的空移动。
- 真实锁屏（SAS 菜单里选"锁定"）：Windows 11 把控制台切到了**新会话 6**（原用户会话 1 变 Disc），服务日志
  `the console session changed; stopping the host in the old one from=1 to="none"` → 1.2 s 后
  `started the host in the console session session=6`，被控端自动跟过去；`info` 报 session 6 / Winlogon / secure，
  `shot` 截到真实锁屏（PIN 框、用户列表），`mouse move`、`keyboard escape` ok，`view` 3 s H.264 28.7 fps。
  **没在锁屏上输过密码**（不做）；解锁走 skill win-dev-unlock（agent 在 Mac 的 Windows App 里点一次 RDP，
  计划任务切回控制台）。
- **重启**（`shutdown /r`）：服务开机自启（日志 `ai-remote service started` → 同秒 `started the host in the console
  session session=1`），但这台开发机配了自动登录，重启后直接进桌面，**观察不到重启后的登录界面**。
- **注销**（`shutdown /l`，等价于没有自动登录的重启）：控制台切到会话 2，被控端跟过去：`info` 报
  `session_id: 2, input_desktop: Winlogon, secure_desktop: true`，`doctor` 的 session/console ok、desktop/approval warn，
  `shot` 截到真实登录界面（时钟壁纸）。之后用 RDP 登录一次，控制台回到用户会话。
- `stop-host`：服务一起 STOPPED、hostw 进程 0；`sc start` 后 `host status` 又是 running。`host uninstall`：服务删除、
  `SoftwareSASGeneration` 回 0、`ai-remote host` 防火墙规则 0 条、无 hostw 进程；开发机恢复到安装前。
- 结论：AGENTS 里锁屏专项 Windows 侧的"未测"清单已全部真机过了一遍；全量 `cargo clippy -D warnings` 和
  `cargo test --workspace`（236 passed）在 Windows 上也过。还没做的只剩 Mac 的注销 / 登录界面（会杀掉开发会话，
  且系统级 LaunchAgent 要 sudo）。
- 当时写下的排查方向（已走完）：把 `OpenInputDesktop` 失败从 debug 提到 warn 并带 OS 错误
  （或设 `AI_REMOTE_LOG=ar_host=debug` 重启服务看日志）；核对 `move_pointer` / enigo 实际在哪个线程调 API；
  核对被控端进程的 integrity level 与 shell 子进程是否相同。
- 没跑：重启到登录界面、`stop-host` 连带停服务、`host uninstall` 恢复策略。开发机上服务**仍装着**
  （`ai-remote-host-cc166525`，SAS 策略已改为 1），不再需要时在管理员终端 `ai-remote host uninstall`。
- 跨机器连通性（原列在"连通性"一节）：跨网段直连已验证；mDNS 跨不了网段，本次没法测同网段发现；强制中继未测。

## 连通性：尽量无感连上，走中继时明确提示（2026-10-12 起，分支 `feat/connectivity`）

目标：开源工具给很多人用，配对后各种网络里都尽量不用配置就能连上；打不通直连、走中继时，各端明确提示
"P2P 直连失败，经 n0 公共中继转发"。n0 公共中继免费但有限流，官方定位开发测试。

### 已做（默认开启，`[network]` 可关；用户文档在 `docs/operations*.md` 第 12 节）

- **中继提示（四端 + 命令行 + MCP）**：`ar-net` 的 `Mesh::route` 返回 `Route { path, relay }`，`is_n0_relay` 按 iroh
  `defaults::prod` 的四个主机名精确比对（防 `*.n0.iroh.link.evil.example`）。controller 的 `hosts` 和 MCP `list_hosts`
  多了 `relay`、`relay_n0`（只加不改）。桌面端：会话工具栏橙色提示 + 悬停说明，标题栏/设备页"n0 公共中继"，浮条保持短词
  （长词在窄窗口被截断）；demo 模式的 lab-pc 就是 n0 中继。iOS（iPhone/iPad 同一套 SwiftUI）：会话期间每 5 s 查一次
  `hosts` 刷新线路（iroh 可能中途升级为直连），顶栏橙色徽章，点开看说明。
- **HTTP 代理**：vendored mcpmesh 加了 `[network].proxy_url`（`boot.rs` 的 `apply_proxy`，记在
  `vendor/mcpmesh/README.ai-remote.md` 和 `PROVENANCE.json` 的 `ai_remote_patches`）。`ar-net/src/proxy.rs` 按
  配置 → `HTTPS_PROXY/ALL_PROXY/HTTP_PROXY` → 系统设置（Windows 注册表 Internet Settings、macOS `scutil --proxy`）识别。
  **只有中继连接走代理**；pkarr 地址发布/查询不走（实测：本地 CONNECT 代理日志里只有 4 台中继，没有 dns.iroh.link）。
- **局域网发现（mDNS）**：被控端默认 `on`，控制端默认 `resolve`（mcpmesh 自己默认 off，理由是公共网络里会广播
  设备 ID 和公网地址；我们为了无感连通改了默认，文档写明并给了关闭方法）。
- **中继兜底**：配了自建中继时默认把 n0 四台中继也写进中继表（`relay_fallback_n0`）。控制端也能配中继了
  （`<profile>/controller/ai-remote.toml`，`ar_core::config::ControllerConfig`）。
- **拨号重试**：`agent.rs` 的 `connection()` 对 `Unreachable` 在 1 s、3 s 后重拨（`retry_after`，已跑满 20 s 的不重拨）；
  拨号改为按主机加锁，不再在拨号期间锁住全部连接（以前一台主机拨 20 s 会卡住其他主机和线路查询）。

实测（本机，单次）：本地 CONNECT 代理 + 配置 `proxy`、或只设 `HTTPS_PROXY`，被控端中继都经代理连上
（`doctor` 的 relay ok）；自建中继配成不存在的地址，回落到 n0 中继；`tools/stream-e2e.py` 1080p30 正常（29.2 fps）。
**未测**：跨机器的 mDNS 直连、真实公司代理、iOS 编译（Windows 上编不了，靠 CI）。

### 下一步

- [x] 打不通的原因诊断（分支 `feat/connectivity-diag`）：vendored mcpmesh 开 `iroh/unstable-net-report` 并加
      `Node::net_report()`；`ar-net` 的 `Mesh::nat()` → `NatReport { udp_v4, udp_v6, symmetric_nat, captive_portal }`
      （故意不带地址），`obstacle()` 给出 `udp_blocked` / `symmetric_nat` / `captive_portal`。被控端 `doctor` 新增 `p2p` 检查；
      控制端 `hosts`/`list_hosts` 在走中继时带 `p2p_obstacle`（本端没问题就是 `remote`，即原因多半在被控端网络），
      桌面端、iOS、命令行的中继说明里写出原因。本机实测 `doctor`：`p2p ok`（UDP 通，无 IPv6）。被控端自己的 NAT 情况
      目前不传给控制端（要走 events 通道），控制端只能判断本端。
- [x] 控制端防火墙（`ar-controller/src/firewall.rs`）：Windows 防火墙开着且有启用的入站阻止规则指向本程序（点掉"允许访问"
      弹窗留下的）时，`p2p_obstacle` 为 `firewall`；桌面端中继提示旁出现"在防火墙中允许"，走 agent 的 `fix_firewall`：
      一次 UAC，提权 PowerShell（`-EncodedCommand`，免引号问题）删掉本程序的入站阻止规则并放行入站 UDP。macOS 只诊断
      （`socketfilterfw --getblockall/--getappblocked`），不改设置。实测：本机防火墙关闭，读取结果 `false`（正确）；
      **阻止规则存在时的判断和 UAC 修复都没实测**（要打开防火墙、点 UAC，属于改系统安全设置）。安装包钩子没做。
- [x] 固定 UDP 端口 `[network] port`（vendored mcpmesh 第三处补丁 `bind_port`）：IPv4 绑定到该端口，端口被占用时
      回退随机端口并告警（测试 `bind_port_is_used_when_free_and_skipped_when_taken`）。
- [ ] 用局域网里的另一台机器做跨机器验证：mDNS（断外网）、强制中继（`unstable-relay-only` 构建）看提示。

## Mac 被控端补全：锁屏 / 登录界面、HEVC、刷新率、光标、iOS（2026-10-20，在 Mac 上做）

这一轮把 AGENTS 里所有标着"要在 Mac 上做"的项一次做完；在一台 Apple Silicon（arm64）、macOS 26.5 的 Mac 上开发，
本机没有 Xcode（只有 Command Line Tools），iOS 侧只能做到 `swiftc -typecheck` 平台无关文件，UIKit 部分靠 CI。

### 锁屏 / 登录界面（`platform/macos.rs`、`install/launchd.rs`、`install/macos.rs`、`ai-remote-hostw --owner-uid`）

- **锁屏**：`CGSessionCopyCurrentDictionary` 的 `CGSSessionScreenIsLocked` 为真时，以前直接 `desktop_usable=false`。
  现在和 Windows 的安全桌面一样：`desktop_usable=true, secure_desktop=true, input_desktop="lock screen"`——
  锁屏画面是同一个 Aqua 会话里 loginwindow 画的，用户自己的进程采得到、CGEvent 也打得进去（RustDesk 就这么输密码），
  只是没人在场回答审批（`prompt_blocker` 照旧拒绝）。`doctor` 的 `desktop` 变 warn。采集侧 `check_access` 没改：它看的
  就是 `desktop_usable`。
- **登录界面**（重启后 / 注销后）：`kCGSessionLoginDoneKey` 为假，这是另一个会话，用户级 LaunchAgent 进不去。
  做法照 RustDesk 的 `agent.plist`：`host install --service` 在 macOS 上写 `/Library/LaunchAgents/<label>.plist`，
  `LimitLoadToSessionType = [LoginWindow, Aqua]`，并删掉用户级那份（两份都在会在 Aqua 会话里起两个被控端）。
  写 `/Library/LaunchAgents` 要 root，所以计划带 `blocker`："run this with sudo"；`sudo` 会改 `HOME`，所以账号从
  **配置目录的属主**（`metadata().uid()`）读，不用 `current_user_id()`（它在 sudo 下是 0；这个函数现在只剩 Windows 用）。
- **登录界面里的身份**：launchd 在 LoginWindow 会话里以 root 起 agent，在 Aqua 会话里以用户起（同一个 plist）。
  以 root 跑会把审计日志、配对文件写成 root 所有，之后用户会话里的被控端就写不了；TCC 的"屏幕录制"授权也是按
  (用户, 可执行文件) 记的。所以 plist 多传 `--owner-uid <uid>`，被控端起来时若 `geteuid()==0` 就
  `initgroups → setgid → setuid`（并验证 `setuid(0)` 失败、改 `HOME`/`USER`），变成安装它的用户；非 root 时只打标记
  `login_window_agent=true`，让 `session_state` 在未登录时也报可用。`AI_REMOTE_KEEP_ROOT=1` 保留 RustDesk 的 root 方案
  作为逃生口。
- **未验证（没法在这台正在用的机器上做）**：锁屏时 ScreenCaptureKit 是否真的出画面、CGEvent 能否解锁；
  `sudo … host install --service` 后注销 / 重启到登录界面，降到用户的进程在 LoginWindow 会话里能否采集、能否输入
  （TCC 对这种进程怎么判，没有把握——所以才留了 `AI_REMOTE_KEEP_ROOT`）。**要人在 Mac 上走一遍**：锁屏 → 控制端看画面
  输密码；`sudo ai-remote --home "$HOME/Library/Application Support/ai-remote" host install --service` → 注销 → 控制端
  看登录界面、输密码登录 → 回到桌面后 `host status` 看 agent 还在；再重启一次。失败先看 `host/host.log` 和
  `host/host-stderr.log`。
- 单测：`platform::macos::tests`（四种会话状态的判定）、`install::launchd::tests`（系统 agent 的 plist 带数组
  `LimitLoadToSessionType` 和 `--owner-uid`）。

### 推流（`stream/capture/mac.rs`、`stream/encode/mac.rs`、`stream/encode.rs`）

- **刷新率**：`refresh_hz` 走 `CGDisplayCopyDisplayMode` + `CGDisplayModeGetRefreshRate`（0 表示不知道 → `None`）。
  帧率上限 60 → 240，长边上限 3840 → 8192（`encode::plan` 再按编码器限：H.264 4096）。
- **HEVC + H.264 High + 低延迟码控**：`HardwareEncoder::new(..., codec)`，HEVC 用 `HEVCMainAutoLevel`、H.264 用
  `H264HighAutoLevel`；先带 `EnableLowLatencyRateControl` 建会话，失败再建普通的。**坑**：低延迟会话不支持查询
  `UsingHardwareAcceleratedVideoEncoder`（`VTSessionCopyProperty` 返回 -12900），之前"M4 上硬件状态查询失败"就是这个——
  `RequireHardwareAcceleratedVideoEncoder` 本身已保证硬件，所以低延迟会话跳过这个查询。周期 IDR 去掉
  （`MaxKeyFrameInterval` 设成 2^30；VideoToolbox 要正数）。`prepare_hardware` 现在和 Windows 一样按观看端的偏好顺序
  逐个试编码器，每个按自己的长边上限算尺寸。`avcc_to_annex_b` 带 `codec`：HEVC 关键帧前面要 VPS(32)/SPS(33)/PPS(34)。
- **在线改码率**：`session.set_properties({AverageBitRate})`，不再重建编码器；失败才重建。
- **测试图案**：`desktop_like` / `scroll_top` / `test_pattern_size` / `still_after` 抽到 `capture::pattern`（两平台共用，
  Windows 的 `win.rs` 改为引用；Windows 侧没编译，靠 CI）。Mac 的 `start_test_pattern` 把滚动画面写进 4 张 IOSurface
  轮换（一张在编码、一张在槽里、两张在写），编码器走和真实采集一样的 `encode_pixel_buffer` 路径。
- **实测（本机，arm64，release，`AI_REMOTE_TEST_PATTERN=1920x1080`，每档 6 s，各 1 次）**：
  `tools/stream-e2e.py`（本机 python 3.9 缺 `ignore_cleanup_errors`，用垫片跑；临时目录要 `TMPDIR=/tmp`，不然 Unix
  socket 路径超长）：

  | 配置 | 编码 | fps | Mbit/s | 首帧 | 帧距 p50/p95 ms | 延迟 p50/p95 ms |
  |---|---|---|---|---|---|---|
  | 1080p30 h264 | h264 | 27.3 | 5.3 | 388 | 34.6 / 42.1 | 23.4 / 38.3 |
  | 1080p60 (balanced) | hevc | 58.0 | 10.7 | 48 | 17.2 / 19.8 | 14.7 / 23.5 |
  | 4K60 h264（图案只有 1080p） | h264 | 58.3 | 23.1 | 58 | 17.2 / 19.7 | 12.8 / 21.6 |
  | native 240 (ultra) | hevc | 98.8 | 30.9 | 45 | 10.0 / 11.1 | 13.1 / 15.5 |

  "native 240" 只到 98.8 fps 的瓶颈没定位（图案线程每帧 memcpy 8 MB、编码器、或刷新率上限，未分开测）。
  硬件单测 `hardware_native_annex_b_roundtrip`、`hardware_hevc_keyframes_carry_parameter_sets`（`--ignored`）过，
  后者顺手把 60 帧 1280x720 的 HEVC/H.264 码流写到 `target/stream-bench/`；
  `node tools/webcodecs-decode-check.mjs "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"`：
  `hev1.1.6.L186.B0` 60/60 帧解出（约 3700 fps，硬解），`avc1.640020`（High）60/60 帧解出（约 47 fps，像是软解或冷启动，
  未深究）。桌面端 WKWebView 的 HEVC 没单独测，WebKit 的 WebCodecs 支持面和 Chrome 不同。

### 光标（`stream/pointer.rs`）

- macOS 实现：位置用 `CGEventCreate(NULL)` + `CGEventGetLocation`（全局坐标，主屏左上为原点，和 xcap 的显示器坐标同一空间），
  可见性 `CGCursorIsVisible`，图像用 `NSCursor.currentSystemCursor`（已废弃但能用；替代品是让 ScreenCaptureKit 把光标画进
  画面，正是这里要避免的）→ `NSImage.CGImageForProposedRect` → 画进 BGRA 位图上下文 → 去预乘、翻转行序。
  **按点而不是像素渲染**（Retina 上光标图 2x，但协议里 `screen` 矩形是点，观看端按同一单位画），hotspot 也是点。
  AppKit 不给光标稳定的标识，所以 shape id 是渲染结果 + hotspot 的 FNV 哈希，每次 poll 都渲染一次（本机 200 次 42 ms，
  即 0.2 ms/次，id 稳定不抖）。请求 `cursor:"client"` 时 `SCStreamConfiguration.showsCursor=false`。
- 依赖：`objc2`、`objc2-app-kit`（NSCursor/NSImage）、`objc2-foundation`、`libc`（setuid 等），都已在 lock 里。

### iOS 观看端（`apps/ios/Sources`、`crates/ar-mobile`）

- `ar-mobile::watch_screen` 请求 `codecs:["hevc","h264"]`、`cursor:"client"`，并把 kind 1/4/5 消息都交给 listener
  （kind 3 pong 丢掉；以前只认 kind 1，其他直接断流）。
- `PhoneModel.swift`：`ScreenHeader`（18 字节头）、`ScreenCodec`、`PointerPosition`（kind 4）、`PointerShape`
  （kind 5 → CGImage，`byteOrder32Little | alphaFirst` 即内存里 BGRA 直通 alpha）；`ScreenFrame` 带 `codec`。
- `H264Decoder.swift`（类名没改）：按帧头编码位走 H.264 或 HEVC（VPS/SPS/PPS →
  `CMVideoFormatDescriptionCreateFromHEVCParameterSets`），编码切换时重建会话。
- `PhoneStore` 按 kind 分发；`NativePhoneViewport` 里光标是 `imageView` 的子视图（跟着缩放平移），位置从 `screen`
  矩形单位换算到图像像素；没图的 shape 0 画一个默认箭头。
- 只过了 `swiftc -typecheck -target arm64-apple-macos14 PhoneModel.swift H264Decoder.swift`；Store / Viewport / RootView
  依赖 UIKit 和 uniffi 生成的 `AIRemoteCore`，本机没法编，**靠 iOS CI**，真机没跑。

### 下一步

- [~] 真机锁屏 **已验证**（2026-10-21，这台 Mac，用户级被控端 + 本机控制端）：Ctrl+Cmd+Q 锁屏后 `info` 报
      `input_desktop: "lock screen", secure_desktop: true`，`doctor` 的 desktop/approval 按设计 warn，`shot` 截到真实锁屏
      （时钟、头像、密码框），`mouse move` ok，`view` 3 s H.264 27 fps（1920x810）。注销 / 重启到登录界面（系统级
      LaunchAgent + 降权）**仍未验证**；按结果决定是否默认 `AI_REMOTE_KEEP_ROOT`。
- [ ] 桌面端在 Mac 上实测 HEVC（WKWebView 的 WebCodecs），以及移动鼠标时客户端光标的收益。
- [ ] "native 240" 档的 98.8 fps 瓶颈。
- [ ] iOS CI 过后在 iPhone 上看 HEVC 和光标叠加。

