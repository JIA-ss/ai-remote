# AGENTS.md

给接手开发的人和 coding agent 的工作笔记。用户文档在 `README*.md` 和 `docs/`；这里只记**当前在做什么、为什么这么做、下一步是什么、踩过的坑**。每次推进后更新本文件，并在提交说明里写清思路。

## MCP 公开目录入口（2026-10-24）

- 中英文 README 首屏并排展示官方 mcpservers.org 收录徽章与既有 M8ven 发布者徽章；导航与 MCP 接入段指向项目的真实公开目录页面。
- 沿用已有深浅品牌插图、客户端配置和权限说明，保留 Pre-alpha、未签名分发与平台 WIP / 验收边界；目录收录不扩大功能或安全保证。
- 本次仅文档更新；已核验徽章 SVG 的 HTTP 200，公开项目页由浏览器另行验收。主干仍通过文档 PR 与原有四项必需 CI 合入，不绕分支保护。

## 通用约定

- 工具链固定 Rust 1.95.0（`rust-toolchain.toml`），`rustfmt.toml` 行宽 120。提交前：
  `cargo fmt --all`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`、
  `node --test tools/desktop-state.test.mjs tools/manual-input.test.mjs tools/stream-ui.test.mjs tools/mcp-install.test.mjs tools/network-ui.test.mjs tools/chat-state.test.mjs tools/chat-render.test.mjs tools/chat-preview.test.mjs tools/chat-folder.test.mjs tools/chat-input.test.mjs tools/chat-codex.test.mjs tools/desktop-nav.test.mjs tools/workspace.test.mjs tools/claude-view.test.mjs tools/smoke-sync.test.mjs tools/codex-sync.test.mjs tools/live-workspace.test.mjs`、
  `cd apps/desktop && npx tsc --noEmit -p .`。
- CI（`.github/workflows/`）在 `macos-15` 和 `windows-2025` 上跑 Rust 测试/Clippy/前端测试；iOS 有单独的 workflow。
  Mac/iOS 代码只能在 Mac 或 CI 上编译，Windows 上改了 `cfg(target_os = "macos")` 的代码要靠 CI 兜底。
- 构建前确认调用目录：Cargo 门槛在仓库根，Tauri 在 `apps/desktop`；非交互子进程需要时为 `PATH` 补 rustup shims（通常为 `~/.cargo/bin`）。
  本次曾在 `apps/desktop` 执行假定仓库根目录的备份脚本，另有构建子进程 `PATH` 未包含 Cargo；修正调用目录和子进程环境后 Tauri 构建通过，旧二进制此前未变。Cargo 并未缺少安装，未改全局 shell 配置，不把这些调用失败记为产品回归。
- 换行符：`.gitattributes` 是 `* text=auto eol=lf`，提交时统一转 LF。Windows 上用脚本改文件优先二进制读写
  （`open(p, 'rb')` / `open(p, 'wb')`）。检查换行用 `git ls-files --eol <file>`；Git Bash 里 `grep -c` 查 CR 会误报。
- Windows 上的 Bash 工具对很长的 heredoc（含引号的大段代码）解析不稳定，大段内容先写成文件再处理。
- 文档写数字时分清"实测（条件、样本量）/ 推导 / 未测"，不要把请求值写成实测值。
- `skills/ai-remote/SKILL.md` 是给 agent 看的操作方法（会话起手、看 / 操作 / 命令 / 文件、级别与审批、拒绝提示对照、机主接管、
  命令行速查）。改工具参数、命令行选项或拒绝文案时同步改它；README 两种语言和 `docs/mcp-clients.md` 指向它。

## 两台开发机装上 main `ccbcf37`，Windows 现场验收（2026-10-26）

- **Mac**：`/Applications/ai-remote.app` 是 `ccbcf37` 的 release，本机自签名证书签名（另一会话 11:52 装的）；被控端装成用户级
  LaunchAgent（安静 + 仅局域网）。新控制端到 win-dev 是 direct（P2P），`chat_runtimes` 可开、Claude Code 2.1.293 / Codex 0.162.1 已登录。
  10-22 旧包备份在 `~/ai-remote-backups/ai-remote-2026-10-22.app`（不在仓库里）。
- **win-dev 现场 e2e**（Windows 11，WebView2，debug `ccbcf37`，窗口在屏幕外、PrintWindow 截图，每套 1 次、第一遍就全过）：
  Codex 替身 121/121、Claude 默认脚本 77/77、工作区 161/161、真实 Codex（0.162.1，ChatGPT 登录，`gpt-5.6-luna`）24/24。
  这是合进 #43 / #50 之后第一次在 Windows 现场跑；之前 Windows 只有 CI（windows-2025）。`tools/windows.ps1 build` 出 release 和
  msi / nsis 两个安装包用时约 16 分钟；安装包本身没装过。
- **win-dev 服务换版**（服务 `ai-remote-host-cc166525`）：路径和参数不变（`host install --service --dry-run` 用新程序比对过，只有目录不同），
  只原地覆盖 `ai-remote-svc-bin` 里的 `ai-remote.exe` 和 `ai-remote-hostw.exe`，防火墙规则照旧；旧版备份在 `ai-remote-svc-bin-backup-20261022`。
  顺序：确认 runner 下没有智能体进程 → `sc stop` → 旧 runner 约 2 分钟后自己退出（没被控端连着、也没智能体）→ 覆盖 → `sc start`。
  **runner 不会被新被控端换掉**（没有版本检查，新被控端会接上还在跑的旧 runner），所以换版前要等它下面的会话都 parked，
  否则智能体还跑旧适配器。这次一开始有一个刚从 Mac 开的 Claude 会话占着 runner，等它自己 parked 才继续。
  换后 `doctor` 全 ok，唯一 warn 是 bypass（Mac 控制端免审批，原有配置）；3 个 parked 会话都还在。服务停了约 2.5 分钟。
- **win-dev 桌面端**：从构建目录复制到 `%USERPROFILE%\ai-remote-app`（不直接跑构建目录，免得重编时被覆盖），两个快捷方式改指过去；
  旧桌面端关窗只是缩到托盘，要强制结束，它的 `ai-remote.exe agent` 不随之退出，用旧 CLI 的 `stop-agent` 停掉。
- **"其他会话"落到设备页（Codex 一节里"没查清的偶发"）查清了，不是产品问题**：那次（`3e69f81`）调试窗口名义上在屏幕外、实际被
  macOS 拉回屏幕内，测试以外的真实点击 / 鼠标侧键落进来，连按两次"返回"：会话页 → 电脑页 → 设备页（深链启动时历史只有一项，
  返回走 `parentOf`）。证据是那次被控端日志结束前约 5 秒新开了一路 960×404 的流，只对得上电脑页概览的预览。路由只在 React
  状态里，所以地址栏不变；`551:34` 是 e2e 第 13 步找不到聊天头的脚本异常，是结果。现在默认离屏的 e2e 已经忽略系统指针、在
  document 捕获层丢掉 `isTrusted` 输入、窗口和任何显示器相交就判失败；剩下的口子是 `--visible` 也放行真实输入（PR #50 的最终 e2e
  就是这样跑的）。本次把它拆开：`--visible` 只让窗口显示并可聚焦，真实输入照样拦；新的 `--native-input`（隐含 `--visible`）才放行，
  输入法测试 `tools/desktop-chat-e2e-ime.js` 改用它。结论来自读代码 + 那一次的日志（对抗式复核两票都没推翻）；"外部输入"是推断，那次没有点击轨迹。
- **模型芯片**：用户反馈"模型选择的按钮窗口没适配"——新建会话页的模型芯片弹的是一个光秃秃的输入框。PR #53（`fix/ws-model-menu`）
  改成和智能体 / 权限芯片一样的菜单（NS-32"模型 · 仅本会话"：默认模型、Claude Code 的别名 Opus / Sonnet / Haiku、用过的模型、
  底部输入框），Mac 真机导航 e2e 10/10、工作区 e2e 161/161。被控端上报的模型列表和 effort 档位由 `feat/ws-model-effort` 接在它上面，
  一起随那条线的集成 PR 进 main。Codex 输入框的模型 / 推理强度芯片等那条线的协议说明。

## 当前交接状态（2026-10-24，P3 已合入）

下方历史章节的“待测 / 未测”只对应当时基准；当前状态以本节为准，后续成功不追认旧失败根因，也不代表全部规格已经完成。

- P1 已随 PR #45 合入 main `028ccc9`，P2 已随 PR #47 合入 main `1aefe9f`；PR #49、#50 随后合入 main `1ddacb1`。**P3 的 PR #48 已合入，最终产品 main 为 `35f54a3bda0fc01d5f0965c404ec0288543f179a`**，GitHub 确认合入时间为 `2026-10-23T21:51:30Z`。产品实现为 `f688335`，`35f54a3` 只补标题就绪预算与记录；本机 macOS 应用 SHA-256 前缀 `4d62b9a6`、CLI 前缀 `3e35eed8`。
- **macOS 最终实测**：选定 16 轮、894 条断言通过，162 张替身安全图全部逐张通过。最后 `35f54a3` workspace 161 项各两轮及 56 张图替换旧 workspace，其余驱动与产品字节相同；primary 78、secondary 64 与补充 20 图不重复累计。真实 Codex 桌面另 24/24、50.014 秒及 6 张新安全图通过，不计入 162 张替身图。Mac 原 PNG 与安全裁图的 SHA、尺寸、完整集合均已本地末次复验。
- **真实与替身被控端基准**：CLI 字节未变，沿用此前替身被控端 35 项各两轮、真实 Codex 被控端受控精确统计 44/44 的证据，没有额外模型测试。旧真实 smoke 43 过、1 失败仍保留；44/44 使用明确串行提示与唯一调用计数，不能追认旧 20 次工具完成的原因，也不把不同链路混成一次测试。
- **Windows 最终实测与合入门槛**：Desktop/core `40024928040`、Native structured UI `40024928047` 两平台四项及独立 Windows 聊天 `40024928145` 全部成功。最终 Windows 合并树 `b3c4464` 的文件树等于 `35f54a3`；六套各两轮共 818/818 条功能断言通过，142 张新安全裁图独立逐张通过，全集、裁图 SHA 与尺寸末次复验一致。Win32 两探针也实际执行，Windows Python suite 23/23、无跳过；原 workflow 记录 `Ran 23 tests in 1.899s`、`OK`。Windows 原 PNG 下载数为 0，原图 SHA/尺寸仅 CI 记录，本地只复验裁图，不能写成本地复验原图。
- **历史失败与未选证据**：旧 Windows `40012531342` 的 816/2、`613cedd` / `fe3f578` 的 `40018173520` 的 812/6 及各自独立 142 图视觉通过继续保留。更早 `f688335` core `40023600939` 被新35取代而取消，不记产品失败；native `40023600860` 成功，Windows `40023600928` 的成功仅作较早回退诊断，其 142 图未选未审，不混入最终集合。旧 `427cfec` / `613cedd` 本机与真实链路证据也按各自基准保留。
- **合入后检查**：main push 生成 core `40029749650` / native `40029749530`；本交接稿形成时未取得终态，未纳入合入前五项成功证据，终态另看对应运行。本次交接文档将走独立 PR，也须等待自身两平台四项必需检查。
- **安装与未测**：main `35f54a3` 尚待 Mac 与 Windows 开发机分别安装并确认，**本会话未安装 P3**（2026-10-26 两台都已装上 `ccbcf37`，见上一节）。PR #50 另会话已有的 Mac release 安装记录完整保留，不当作本次 main35 已装。真实 Claude 未登录，Windows 现场、物理键鼠/IME/辅助技术、动画节拍与性能仍未测；本机只有 CLT，没有完整 Xcode。其余未实现的会话组织与长对话功能见两份工作区笔记。

## Windows 解码故障与批量验收精确化（2026-10-24，PR #48）

- **失败实测**：`613cedd` 的 Windows 聊天运行 `40018173520` 为 812 项过、6 项失败，均在工作区。两轮都实际等满首帧检查的 30 秒，secure context / WebCodecs 可用、没有 session/stream-ended 错误，但画布一直 0×0；整个套件分别记录 502 / 503 次配置和同数 `OperationError`，未记录 decoder caught-up。被控端两次选择了既有 GPU→CPU 采集回退；这些数字只证明配置/错误循环，不证明具体显卡或驱动原因。
- **产品修复**：解码仍先请求 `prefer-hardware`；当前解码器的 `OperationError` / `NotSupportedError` 让同一编码与尺寸在下一关键帧用 `no-preference`，由浏览器选择可用路径。只保留本次推流最近 16 个失败配置，普通 reset 不会再次尝试该配置的硬件偏好；不同配置仍可先用硬件。DataError 等仍按原规则请求关键帧，退役/已关闭实例的迟到错误不污染新实例。`no-preference` 不等于实测软件解码；语义依据 [WebCodecs 7.9](https://www.w3.org/TR/webcodecs/#dom-hardwareacceleration)。
- **键盘验收精确化**：另 4 项失败涉及 inert / 恢复归属 / 元数据快照。旧快照混入异步状态点，且 DOM 不等会短路真正的后端检查；当前证据不能证明键盘改了持久元数据，也不能追认失败由状态点或自动标题导致。现在严格比较 title/pin/archive，状态点另记；后端核对始终执行，两个元数据结果仍严格合取。四个阶段只导出静态名称和布尔，不导出会话 ID、标题或原事件。自动标题与列表必须先对齐同一被控端快照，才开始键盘探针；未就绪即失败，不用 sleep 或重试。
- **最终 Windows 功能实测（`40024928145`）**：workspace 两轮均取得 1024×768 画布，无会话或 stream-ended 错误；每轮记录 `OperationError` 2 次、默认偏好回退 2 次、decoder caught-up 2 次，硬件偏好错误后恢复路径已在该环境实际走通。`no-preference` 仍由浏览器选择，不等于证明软件解码；检查点等待 0 ms 也不是流启动时延。四阶段 ownership 的 flow/selection/current/metadata/menu/focus/inert 与 host 元数据布尔两轮全真，状态点也未变；旧 812/6 的具体触发条件仍未追认。
- **就绪预算推导**：后台工作区合法轮询间隔为 20 秒，挂载时的即时刷新不代表自动标题也已到达；因此新增标题/列表对齐门槛在隐藏文档中共用 25 秒、可见文档中共用 10 秒。25 秒是轮询间隔加工程余量的推导上限，不是实测标题时延或网络保证；仍按实际快照就绪立即继续，超时失败，独立解码帧的 30 秒检查不变。
- **静态阶段与后续实测**：新增 7 个真实模块回归覆盖同步/异步失败、数据错误、迟到回调、配置隔离、有界历史及输出/清理；全量 Node 423 项中 422 过、1 平台项跳过，TypeScript 通过。该阶段 Windows 证据便携测试在 Mac 为 23 项中 21 过、2 个 Win32 项跳过；随后最终 Windows workflow 实际执行 23/23，无跳过，记录 1.899s。Rust 源码未改，静态结果本身不证明 WebView2 恢复；最终 Windows 两轮恢复与 818/818、142 图通过属于后续实际证据。P3 已合 main `35f54a3`，旧失败及未知根因保留，本会话未安装。

## 并行主干整合与重新验收（2026-10-24，PR #48）

- 并行会话已把 PR #49、#50 合入 main `1ddacb1`。其中包含 MCP 元数据与原生夹具报告修正，以及推流旁的完整工作区、新路由、菜单、卡片焦点与对应 E2E。此前只 fetch 后推送、未及时检查 origin/main 是否为本分支祖先，漏查了合并冲突，导致 PR 的 pull_request CI 没有生成检查记录；不是工作流被禁用。
- 本次先整合主干，再保留本批多选/批量/撤销、动效回退、摘要语义等待与 Windows 证据入口。共享菜单按可见后聚焦；嵌入侧栏的多选快捷键应遵循工作区键盘归属，画面接管或 pane inert 时不能截走按键。
  Windows 证据只裁完整工作区 pane 或安全对话框，不能把 LiveWorkspace 的视频半边当作测试 UI。四份 quiet 配置、只读 invoke 兼容、失败保留与精确计数仍是门槛。
- 旧 `40005026321` 聊天运行及手动触发的旧 `4bcc35e` core/chat 运行因整合主干而主动取消，不能记整合版成功或产品失败。原生 `40007638843` 另保留自身的实际结果；即使通过也不替代新合并树验收。
- **整合态静态/便携实测**：TypeScript、fmt、全量 Node 416 项（415 过、1 平台跳过）通过；Windows runner 16 项在本机 14 过、2 个 Win32 探针跳过，fixture reader 4 项通过。新增 pane 测试确认导出的视频色块为 0、隐藏/零矩形时拒绝父层回退；这些不替代真实 WebView2 或原生像素验收。
- **整合源码 `427cfec` 实测**：本机 Clippy、Rust 440 过 / 9 忽略、CLI 与 Tauri debug 构建通过；被控端替身 35 项各两轮通过。原生六套各两轮：工作区 161、默认对话 77、Codex 121、渲染 12、离线 7、预览 31，每轮合计 409，两轮 818 项全过；补充 Claude 细节 28 与导航 10 各两轮，另 76 项全过。142 张主图和 20 张补充图均已逐张复核。完全离屏、非持久配置、四份 quiet 与真实退出均有记录；隐藏文档里的 DOM 快捷键探针不代表物理键鼠验收。
- **新增诊断的本机补验**：`613cedd` workspace 161 项连续两轮通过，56 张新安全图已逐张通过；两轮类型化记录为3840×1620、hidden/secure/WebCodecs均为true、错误状态均为false，检查点等待0/1 ms不是流启动时延。被控端替身35项又各两轮通过（10.257 / 9.794秒）。清理检查仅确认本任务debug进程0、验收目录复制auth文件0、临时真实profile 0，不推导系统没有其它真实应用或CLI。
- **真实 Codex 与失败保留**：同一整合包、Codex 0.159.2、已登录、模型 `gpt-6-luna` 的桌面流程 24 项通过，6 张图复核通过（6 个文件、5 个唯一安全裁图内容）。被控端 smoke 43 项通过、1 项插话行为失败：同轮插话约 0.03 秒获应答，末答为 5，但旧统计记 20 次工具完成。代码链确认上游成功应答后才发插话用户事件；现有证据不能确定是否提前排了命令，也不能认定产品丢插话。测试补明确逐命令等待、按唯一 execute call_id 计数、发送返回码及可选私有逐事件留证；保留原 `<12`、末答 5、同轮要求；`613cedd` 新受控复验 44/44 通过、43.063 秒，使用同一产品二进制。提示与统计方式已变，不追认旧失败根因。
- **精确插话留证复核**：新运行实际执行`sleep 1; echo 1`至`echo 5`，第2条完成后发插话，初始与插话发送返回码均0；发送/应答时各有2条已开始且完成，同turn6应答后才出现第3条开始。36个带序号事件连续、无重复（另1个无序号commands快照），转录同时未完成的execute最多1条。脚本观察发送至应答29.044 ms，host第2条完成至应答事件时间差28 ms；它们不是模型内部读取时点，不能证明内部没有预生成，也不反推旧失败原因。原隔离profile已删除；清理结论只限本任务范围。
- **Windows 仍未验收通过**：`40012531342` 的 12 轮中 816 项过、2 项失败，均为工作区“已收到解码帧且无采集错误”；其余五套各两轮全部通过。截图只导出安全工作区与预览裁图；旧证据未导出 canvas 尺寸或错误分类，不能区分采集、传输、解码或终止。新增静态枚举/布尔/数值诊断与私有原生日志白名单计数，禁止导出错误原文、原屏幕或任意日志；原断言与 30 秒等待不变。便携诊断测试本机 20 项中 18 过、2 个 Win32 项跳过，含实际驱动事件到解析器的契约检查；该轮142张安全裁图后续逐张视觉通过，功能816过/2失败仍保留；新运行结果见下一条。
- **最新 Windows 失败（`40018173520`）**：`613cedd` 触发的PR合并树`fe3f578`共12轮，812项过、6项失败，全部在workspace：第1轮metadata不变和首帧两项失败，第2轮另有inert/恢复ownership两项失败；其余五套各两轮通过。两轮检查点canvas均0×0、connected=true、secure/WebCodecs=true、hidden=false，无session-error/stream-ended；30秒等待实际30065/30098 ms。解码器配置/OperationError计数分别502/502、503/503，caught-up均0，host GPU→CPU fallback各2次。这些是白名单诊断计数，不是已解码帧数；正在验证硬件偏好失败后重复按原配置重建的路径与最小回退。ownership失败另行核对异步状态和真实事件，不无证据归因解码压力。新142张安全裁图已独立逐张视觉通过，裁图SHA、尺寸与完整集合末次复验通过；功能812过/6失败保持，原图未上传、原图哈希仅CI记录。
- **该阶段必需 CI 与后续**：`427cfec` 的 Desktop/core `40012531326`、Native structured UI `40012531320` 两平台四项全部成功，但该阶段独立 Windows 聊天失败当时仍阻止 PR #48 合入。旧816/2与新812/6两轮失败均保留；后续解码回退与ownership验收精确化见上节，后续最终35的Windows818/818与142图通过另记于顶部。旧阶段图集按各自基准保留，不追认为新源码；真实 Claude 已安装但未登录，本轮未测。此会话未安装 PR #48 候选到 Mac 或 Windows 开发机；下节 PR #50 的另会话安装记录保留。

## 实时画面与完整智能体工作区并排（2026-10-24，PR #50）

- 用户明确要复用现有完整工作区：左实时画面，右侧保留 CLI、工程、会话、新建和对话；不再增加顶部选择条，也不跳另一套配置页。
  有已配对电脑时，智能体和看电脑入口都进入同一并排视图；工作区选择电脑时同步左侧，选择同机项目/会话只改变右侧。
- `LiveWorkspace` 让左 `SessionPage` 与右 `Workspace embedded` 成为兄弟节点，只给左侧按 host 加 key；中间可调整宽度。
  工作区按自身容器测宽并使用原侧栏/抽屉，保留完整 computers 给全局 store。新建地点回报不清草稿、不循环；已有首句和一次性 ticket 仍用原流程。
- 快捷键限右侧，Claude 审批/计划/问答、数字会话导航及模式菜单不截左侧远程输入；右侧已聚焦控件卸载、焦点回 body 时仍保留其按键归属，
  左侧指针/焦点动作立即接回。全屏隐藏右侧但保留会话，Review 等父层解除 inert 后才聚焦。
  隐藏应用停止画面与远程输入，重新显示后等新几何和首帧再恢复；不能因工作区默认并排而在后台重新观看。
- 从电脑页打开的旧 `chat.session` 也按最近工作区恢复；切到设备页再回来保留当前会话，旧 ticket 不复用。
  已打开的会话断线时保留正文、草稿、未送达和重试；首次离线、权限拒绝和会话不存在仍显示原有明确状态。
- **门槛实测**：本次界面源码 `b8068e9`，之后本分支只改测试和记录。Rust fmt、全工作区/all-targets Clippy、Rust 439 过 / 9 忽略、
  TypeScript、CLI / Tauri debug 构建与 release app 打包通过；全量前端 399 项，398 过 / 1 平台项跳过。
  本地 Rust 全量在本分支初始基准通过，后续同步 main 只带 Windows fixture 诊断的 Rust 差异；最终两平台 CI 另行核对，不把 Mac 当 Windows 现场。
- **最终本机原生 E2E**：同一份最终 debug 应用、隔离 quiet 被控端和控制端，经真实 QUIC 与替身/真实 CLI，以下每套各一轮全过；
  请求 `--visible`，实际 1280×800 CSS 窗口、DPR 2，文档报告 hidden / focused。默认完全离屏首轮因显示器枚举空被硬门槛拒绝，
  本轮不冒称完全离屏或物理键鼠验证。

  | 脚本 | 通过项 |
  |---|---:|
  | workspace | 129 |
  | Codex 替身 | 121 |
  | Claude 默认 | 74 |
  | render | 12 |
  | preview | 31 |
  | offline | 7 |
  | 真实 Codex | 24 |

  真实 Codex 0.159.2、ChatGPT 已登录、`gpt-6-luna`，覆盖真实修改、命令、改动卡、插话、停止与按原 ID 重开；
  校验返回 2026 年四个正确季度末日期。工作区关键同机新建/切换/历史段实测 1417 ms，已解码 3840×1620、同一画布，原始审计无 view_start/view_end；
  此前两个阶段另有 129/129 各一轮通过，单独保留，不混成最终源码三连。Camofox headless 深浅主题、1440 / 1280 窗宽、窄左画面及跨机画布切换已看过，未见文档横溢出。
- **保留的失败与修复**：早期原生第 1 轮在 51 项中有 2 项快捷键归属失败，修复 body 焦点归属；第 2 轮 128 项有 2 项失败，
  原因是新测试创建第 5 个智能体撞既有 4 个额度，改为先结束本轮自建空闲会话，未放宽服务上限。
  Codex 初轮 101 项中有 3 项菜单/停止失败：菜单首次在隐藏测量阶段聚焦无效，改为可见提交后聚焦；新增真实组件回归在旧码失败、新码通过。
  Claude 初轮 72 项虽通过，逐图仍发现目录弹层被玻璃卡片样式变半透明、输入框文字透出；恢复原不透明弹层，最终两处 computed alpha 均 255 / opacity 1。
  preview 初轮 29 过 / 2 失败是驱动从旧存储键取不到 session，已改读取实际选中工作区会话；原两条安全拒绝断言完整保留。
  两个 Codex 驱动同时按完整工程路径确认选择；所有旧失败日志/截图留在仓库外，不删除、不追认为通过。
- **视觉复核**：最终七套 73 个 PNG 文件清单、SHA-256 与逐图结论均已核对，未见剩余确证遮挡/溢出；旧的两张目录透字图单独保留。
  历史返回的一张截图拍在加载阶段，只证明绘制状态，不能写成回放追平或终态；完整回放另由 Codex / Claude 套件的内容与轮数断言验证。
- **安装与边界**：最终 release 桌面端已原子替换 `/Applications/ai-remote.app`，安装前核对旧版指纹，安装后 deep/strict 签名与三个可执行文件 SHA-256 均核对，
  旧 app 留作本地备份；未迁移或重启既有被控端服务。本轮只测本机，Windows 现场、真实 Claude 登录、物理键鼠/IME/VoiceOver 未测；无 iOS 改动。
  安装版实际打开，智能体入口和新建页正常；正式配置当时 0 台已配对电脑，显示配对提示，没有将这次启动当作正式配置里的实时画面验收。
  独立 worktree 实现，主检出其它会话的页面美化和 iPad 改动未纳入。
- **本地合入**：PR head `8571970` 的两平台 test/native 四项检查全成功（Desktop/core `40001419318`、Native structured UI `40001419198`）。
  推送前发现 main 新增 `c9d03ec`（MCP 元数据及 fixture reader），重新在临时 worktree 从它 `merge --no-ff`，完整保留两边记录；
  界面文件与 `b8068e9` 完全相同，合并态另跑 fmt / workspace Clippy、Rust 440 过 / 9 忽略、13 项 mounts 检查与 reader 4 项回归均通过。
  这批独立元数据变化没有改会话或推流，未将七套界面验收无意义地重复计数；main 合入后的自动 CI 单独核对。

## 智能体工作区第二阶段 · 第三段：列表多选与批量操作（2026-10-23 ~ 24）

- Cmd/Ctrl 点击切换选择，Shift 按可见行顺序选择范围；选中行右键打开批量菜单。Esc、无修饰点击行外（含组头与列表空白）或普通打开会话会清空选择，选择不持久化。
  全选至少需要两个会话；有折叠分区时禁用并提示先展开。保留默认的文件夹与电脑分组、截断后的“更多”和 Ctrl+Shift 数字跳转。
- 只接现有置顶、已读状态和归档接口；运行中或待处理的会话不能批量改已读。菜单字母只作用于当前菜单，多选时阻止单行快捷键误改其中一行。
  批量远端请求共享并发上限 8，同会话编辑串行、每批按会话去重；逐项收集成功与失败，部分成功明确计数并说明失败原因。
- 撤销只处理实际成功且状态仍符合本批结果的项，只发本次字段的补丁。旧请求的回复与回滚也只改本次涉及的字段，保留期间收到的其他元数据。
  本地已读批量只更新一次快照，每个既有存储键写一次；打开会话后的新决定优先于撤销。归档后被筛掉的成功项仍可撤销回来。
- 修复共享弹出菜单首次聚焦：先完成隐藏测量，再在显示后聚焦，避免浏览器拒绝焦点而让字母键仍发给列表；保留菜单的筛选及确认步骤更新不再触发重新聚焦。
- **开发期浏览器实测**：Camofox headless 演示后端 24 项页面流程通过，深色中文 / 浅色英文截图看过；DOM 事件触发真实挂载组件，
  不能当作系统键鼠证明。开发初期前端 369 项（368 过、1 跳过）；整合后全套 391 项（390 过、1 平台项跳过）、TypeScript 通过，保留不同阶段口径。
- **门槛实测**：产品基准 `59c86aa` 的 fmt、全工作区/all-targets Clippy、Rust workspace 测试通过（439 过、9 忽略，另 13 项 mounts 检查）；
  Tauri debug/no-bundle 构建成功。后续整合到 `fc7fe3d`（原 `6d46f13` 只更正祖先提交署名，文件树一致）没有生产 TS/Rust 差异，新增 Rust 仅为上一节的原生夹具诊断测试。
  原生记录保存实际二进制与脚本哈希，不能把不同验收工具版本说成同一轮。
- **修复前基准的本机原生实测（历史）**：实际应用到本机隔离被控端，四份 quiet 配置、非持久 WebView、当轮窗口与报告的全部显示器无交叠；
  workspace **134**、Codex **120**、chat **72**、render **12**、offline **7**、preview **31** 项，各连续两轮通过。
  新批量段以三个非当前空闲会话为样本，验证修饰键多选/范围、菜单焦点、P/U/A、撤销；置顶与归档另从真实被控端 `chat_sessions` 查询持久化。
  8 是程序并发上限，Node 模拟验证此上限；本机原生三个会话样本不冒称实测八项并发。
- **整合验收工具后实测**：同一产品二进制、脚本基准 `faed828` 的 Claude details 28 与导航 10 项，各连续两轮通过；
  真实 Codex CLI 0.159.2 / `gpt-6-luna` 23 项通过，含真实文件修改、命令校验、插话、停止与原会话回放。
  独立核对生成文件的四个 2026 季度末日期，4/4 正确。最后的只读 IPC 接入修正仅在 Windows CI 模式生效，本机普通路径不接该 helper。
- **修复前基准的逐图复核（历史）**：本批六套替身 136 张、附加 details/navigation 20 张及真实 Codex 6 张，162/162 张新图逐张通过；
  实际集合无遗漏/重复，原 PNG SHA-256 与尺寸全部末次复验一致。推流截图只审右测试会话栏，预览只审对话框，不审或发布真实桌面。
- **字体实测与限制**：两轮 WK 都返回打包 `Geist Mono` 的非空 FontFace 且全部 loaded，5 秒是共享等待预算而非实测耗时，
  这证明资源可加载，不证明每个字形排他使用该字体。早期 Camofox 资源虽返回 200，`fonts.load` 仍报 `NS_ERROR_FAILURE`，
  那批浏览器图存在回退条件，原因未定位，不能由 WK 的成功追认浏览器字体已通过。
- **未测 / 装机**：真实 Claude 未登录，真实审批/提问/计划/标题/断线整段未测；本机 Windows 现场、物理键鼠/IME/触屏/读屏器、OS caption 与动画性能未测。
  本机只有 Command Line Tools，没有完整 Xcode，本会话未运行完整 iOS 验收；其他章节的 CI/iOS 结果仍只属于其记录的基准。本批 P3 未安装到开发机；PR #50 另会话的安装历史独立保留。
- **当时下一步与现状**：该阶段曾等待 PR #48 修复后的原生、Windows 全矩阵、新图及必需检查；后续已按最终35基准完成并合入。旧功能全绿但视觉失败的运行仍保留，安装待办以顶部为准。
  自建分组、拖拽、子会话树、深入搜索等仍在 not_done，本段不声称全部还原完成。

## Windows 原生聊天 CI 验收入口（2026-10-24，P3）

- 新增独立 `windows-chat.yml`：限定路径的 PR 触发和手动触发，Windows 2025；Node 22.16.0、Python 3.13.2、Rust 1.95.0。
  90 分钟硬上限，只 restore 现有 desktop Rust 依赖缓存，不 save、不改变四个现有 required job 的超时或缓存策略。
- 构建 CLI 与 Tauri debug/no-bundle 后，六套替身脚本各固定运行两遍；第一遍失败不会被第二遍覆盖，也没有自动重试。
  每套 fresh temp、四份 quiet 配置、非持久 WebView、离屏几何、App/harness 退出码和精确断言计数均作为成功条件；quiet 不代表禁止全部网络。
  Job Object 先纳管暂停的 harness 再恢复，整体超时或正常退出后清掉该 scope 的残留进程，不按进程名清理其他任务。
- 上传产物仅字段白名单摘要与安全测试 UI 裁图。裁框来自截图前 DOM 和同一进程的原生客户区物理坐标；LiveWorkspace 只保留完整测试工作区 pane，旧聊天布局只保留聊天栏，预览只保留对话框并遮掉圆角外像素；不包含视频舞台。
  原 PNG、原日志、sessions、homes/auth、WebView 与 drive.js 不上传，也不把 harness 原文送到 Actions console。
  CI 模式在发出 done 前采集自动终帧布局及独立断言计数；每张命名/终态截图都须有可靠裁图，缺失时明确未验并使该套失败。有效像素检查不等于图审，Windows 裁图须下载后逐张验收。Actions artifact 不称为私有。
- **本地实测**：9 项便携 runner 行为测试、Python/JS 语法、布局 hook 语法和 YAML 解析通过；前端 391 项（390 过、1 跳过）。
  静态对抗审查确认 Tauri 2.12.2 的内部 invoke 只读，CI 改用独立可写 helper，仅重接已知驱动适配器；缺少唯一入口立即失败。
  portable VM 使用真实只读 descriptor，验证 native 函数未改、业务参数/返回保持，以及 done 前终帧布局与计数完整。
  2 项 Win32 Job 运行探针只在 Windows 执行，本机跳过。以上是该入口的本地静态阶段，不能据此记 Windows 已通过；云端运行及逐图结果另行追记。
- 首次云运行 `39988590929` 为更正尚未合入分支的提交署名而由本会话主动取消；WebView2 六套矩阵步骤未执行，不能计成功或产品失败。
  更正只涉及提交元数据与交接引用，原实现文件树已逐项确认相同；新提交须重新取得必需检查和独立 Windows 验收结果。
- 随后运行 `39989630714` 已通过运行时/Win32 探针与 CLI 编译；Tauri 的 npx 包装调用报 `unexpected argument --locked`，
  六套界面矩阵未执行。构建改用已安装的 Tauri Node 入口直接传参，另在编译前用原生 Node 验证含 Cargo 分隔符的完整参数数组；
  保留 `--locked`。这是构建入口修正；后续 `39992295300` 已完成矩阵但视觉未通过，见下一节，不能把构建成功当作产品验收通过。

## Windows 扫光文字视觉回归（2026-10-24，PR #48）

- Windows 运行 `39992295300`（分支 `fa62129`）在 WebView2 `155.0.4283.62` 上实际完成六套替身各两轮：chat 72、render 12、offline 7、preview 31、Codex 120、workspace 134 项；合计 752 条断言，全部成功，无自动重试，136 张安全裁图完整。
  所有 App/harness 退出码、隔离、离屏几何和 scope 清理通过；9 项便携测试与 2 项 Win32 探针在 Actions 日志中确认全部实际执行，11 项通过、无跳过；裁图 SHA-256、尺寸与集合下载后复验。原图不在 artifact，本地没有复验其哈希，不能把 CI 记录的原图哈希说成本地复验。
- **视觉未通过**：136 张裁图逐张复核发现 8 张问题：两轮 chat 的 queued/stopped 共四张及 workspace queued 两张活动标签缺字、只剩灰色渐变矩形；另两张 Claude thinking 带异常灰底。
  最初将其中两张误认成合法过渡的图审结论已撤销。功能全绿不替代逐图验收，该提交没有合 main。
- 对照：已有 macOS 同场景文字可读；隔离 Chrome 155 的最小静态夹具在默认偏好下未复现，强制系统减少动态效果后精确复现嵌套标签缺字、纯文字带灰底。
  已查明的产品缺陷是浅色渐变选择器优先级高于 media 回退，渐变未清掉而裁剪已重置为 border-box，内部文字又仍透明。旧 Windows 运行没有记录 media 值，不能据相似截图追认它当时的实际偏好，也不能据此断定 PrintWindow 是根因。
  `8486866` 仅调整 CSS：主题差异改为高光变量，系统与应用的减少动态效果回退恢复父子文字并清除渐变，保留代码对象浅底、文本 DOM 与正常扫光功能，截图准备规则未改。
- **修复后本地实测**：编译后 CSS 的 Chrome 155 最小夹具，浅/深主题 × 应用减少动态效果开/关 × 系统偏好默认/强制减少，共 8 组全部通过；正常计算样式周期为 3s、动画状态为 running，回退无渐变、父子文字均有色、代码浅底保留，两张夹具图已查看。3s 是 CSS 计算样式值，不是逐帧或墙钟周期实测；动画节拍与性能仍未测，该夹具不代替真实 Tauri 验收。前端 391 项（390 过、1 平台跳过）、TypeScript 与前端构建通过。
  同步修正正常扫光的实现参数；该独立对齐不是灰块故障的必要修复。
- **Windows 证据入口补强**：本会话已主动请求取消 `39998869206`，原因是 JS 记录的系统/应用动效偏好仅进入未上传的日志，公开的类型化证据无法复核该条件；这是主动终止待验运行，不记为产品失败或成功。工具提交 `ef3fa3a` 已补偏好字段白名单与缺失即失败的断言，本地 14 项中 12 过、2 个 Win32 探针按平台跳过；Windows 运行 `40000053673` 的 14 项测试全部实际通过、无跳过；聊天摘要的四条失败另见下文。产品 CSS 与本机原生二进制基准仍是 `8486866`。
  quiet 配置 helper 的独立 AST 执行已验证两份新 home 下合计四份配置与 LF 字节；没有启动原生 GUI，也不当作 doctor 锁已通过，后续 CI 继续验证该门槛。
- **修复后首批本机原生实测（摘要等待修正前脚本）**：同一 `8486866` 调试包，chat 75、render 12、offline 7、preview 31、Codex 120、workspace 134 项各连续两轮；另 Claude details 28、navigation 10 项各连续两轮。合计 16 轮、834 条断言全过，158 张新图逐张通过，原 PNG 和安全裁图的哈希、尺寸与完整集合末次复验一致。
  macOS chat 两轮实际系统偏好为不减少动态效果，正常扫光计算样式周期 3s、running；测试临时设置根属性 `data-reduce-motion=true` 后父子文字有色、无渐变与动画，恢复原属性后复核正常；这是 CSS 条件验证，不是设置页面开关操作。测试进程已退出，临时浏览器配置已清理，任务证据目录无复制认证残留。
- **待验证**：Windows 最终运行、全部新裁图及最新提交的四项必需检查；没有证据前不记整批验收通过。

- Windows 运行 `39998869206` 为补齐可复核的系统动效偏好而主动取消，不记产品失败或完成。原始偏好只进隔离日志，原 typed 摘要没有保留；现仅白名单导出 `systemReduce` 布尔和 `appReduce` 的 null / true / false 字符串，不带文字、命令、颜色或其他原始字段。
  chat 两轮各要求恰好一条合法偏好记录，缺失、重复或坏类型均使该轮失败；其它脚本没有该事件，不凭空要求。用实际 macOS 75 项原生日志独立核对，解析得到 systemReduce=false、appReduce=null；Windows `40000053673` 两轮 chat 已实测 systemReduce=true、appReduce=null。

- Windows 新运行 `40000053673` 实际执行 12 轮、758 条断言，其中 754 过、4 失败：chat 两轮各在最初两项工具组摘要检查失败，其余五套全部两轮通过；138 张裁图完整。chat 两轮实际 `systemReduce=true`、`appReduce=null`，新增动效三项均通过，关键运行标签文字可读、旧灰块消失。
  后续查找步骤的裁图可见完整摘要及“1 failed”，不属于永久缺失。结合已通过的统计/折叠断言与源码推导：回合 footer 出现时，组头仍可能处在 650 ms 最短停留窗口；650 ms 是实现约定，不是本次云失败瞬间的实测耗时。
  `dwellStep` 的受控终态 null 验证确认旧标签保持到截止、截止后正常清空；测试改为等待同一回合的真实 `.cl-sent` 非空，再执行原有两条独立断言。不加任意 sleep，不扩大既有等待预算，不改产品停留规则，不用重试覆盖失败。新脚本 `5eea269` 本机 chat 两轮各 75 项已通过，同一 `8486866` 产品二进制未改；两轮最初已有摘要、等待计算值均为 0 ms，因此不声称本机复现旧 Windows 早读窗口。38 张新图后续已逐张通过；两轮初始摘要均已存在、等待值为 0 ms，未复现旧 Windows 早读瞬间。旧 Windows `40005026321` 后因整合主干主动取消，不记产品成功或失败。
  该阶段最终本机集合采用未改源码的其它五套 100 张、补充细节/导航 20 张与最新 chat 38 张，共 158 张，已逐张通过；新 chat 替换旧 38 张，不重复累加。它不替代后续 `427cfec` 整合树的独立图集。

## Windows 首次 UIA 读取的空错误诊断（2026-10-24，PR #48）

- `5eea269` 的原生运行 `40005026322` 在 Windows 第一轮、首次 `ar_ui::collect` 返回错误，6.72 秒结束；startup poll 已在 5904 ms 命中、子进程仍运行，后续输入/点击/开关动作没有执行。这是与旧 15 秒 generic poll 超时不同的失败。
  夹具 stage 记录已到 form_shown / first_tick、stdout/stderr 为空。不能据启动耗时认定启动慢；当前空 Display 没有保留调用阶段或数值码，底层原因仍未知。
- Windows provider 的初始化、walker/root 与根级 sibling 错误现在补静态调用阶段、十进制与十六进制数值码、有界原文，并保留源错误供 downcast；不改 no_element 集合、动作语义或 500 ms COM 超时。
  原生夹具首次 collect 失败时另存 `collect-error.json`，只含阶段、耗时、子进程状态、错误元数据及受控夹具状态，不保存其它桌面节点或窗口标题。合成空消息的格式单测所用错误码只是测试常量，不代表本次实际失败码。
- 同步补齐旧 `ui-smoke.py`：启动两端前以二进制 LF 写齐两份隔离 home 各 host/controller 的四份 quiet 配置，并要求远端 doctor 确认 quiet 锁定。之前默认关闭发现不替代这四份显式配置；本机 AST 独立执行已核四份精确 LF 字节，原生 CI 结果待测。
- 本轮保留两次预先规划的原生命令，第一轮失败即停，不以重试洗掉失败；新增诊断只解决空错误不可定位，不声称底层 UIA 原因或旧 poll 超时已修复。fmt、Clippy、测试及新 CI 待完成。

## Windows 原生 fixture 失败诊断（2026-10-24，PR #47）

- 首次 Windows UIA 原生测试在 15.46 秒后仅报 `fixture state verifier timed out`；四个等待点共用文案，原临时目录已删除，
  所以**根因仍未知**，不能根据总时长认定是启动慢、Timer 或 UIA 动作。`ar-ui` 测试/provider 与 fixture 相比已成功的 `028ccc9` 没有改动。
- 这次只增加可证伪的诊断：四个 poll 使用 `startup_pid` / `set_value` / `press_apply` / `toggle_option` 静态阶段，记录 elapsed、
  最后成功解析的受控 fixture 状态、最后解析或 IO 错误及 `child.try_wait` 状态；子进程退出时立即失败。
  状态、逐阶段 verifier 记录、fixture stdout/stderr 保存于 `target/ui-validation/native-<pid>-<timestamp>/`，不随临时编译目录删除。
  Windows fixture 另记 bootstrap、assemblies_loaded、form_shown、first_tick；不记录密码框、环境变量或其他应用内容。
- CI 预先规划同一原生命令连续两轮，每轮写 planned-round 标记并追加日志，第一轮失败立即退出，**两轮都过才通过**；不是失败后重试。
  保留 15 秒等待、全部业务断言及 provider/Swift 源码。macOS 对诊断源码（`a318b07`；原 `9ee03ec` 仅更正作者/提交者署名，文件树完全一致）的 fmt、全工作区/all-targets Clippy 通过；
  `cargo test -p ar-ui --locked` 编译通过、0 项执行 / 1 项原生测试按原设置忽略，不能当作 GUI 实测。以下保留新增 Windows 两轮采样的后续结果；
  首次失败的根因仍未知，后续成功也不追认它为已查明。
- PR #47 诊断运行 `39987649385` 的 Windows real UIA fixture 步骤已实际连续两轮通过（原生单测每轮 1 项），
  后续必需检查完成，PR #47 已合入 main `1aefe9f`；未因后来的通过删除首次失败，也未扩大超时或修改业务断言来消红。将来若再现，继续据阶段与留存状态定位。

## MCP 聚合入口元数据（2026-10-24）

- `list_hosts` 明确声明只读、非破坏性、幂等；`use_host` 会改本会话的默认被控端，所以不是只读，重复选择同一端是幂等的。
  两者均可能查询已配对的外部主机，`openWorldHint = true`。这些是客户端提示，不改变原有配对、等级、审批与审计门禁。
- 新增真实 rmcp `tools/list` 协议回归：全新临时 profile、未配对、LAN-only、关闭发现和映射，不使用机主设备或本机服务。
  测试先在原代码上因缺 annotations 失败，再补齐两项本地工具的四个布尔 hint。根 `glama.json` 仅声明维护者供目录核对。
- 中英 README 的 20 px 紧凑 M8ven badge 保留平台本次领取的公开所有权核对参数；这不是 Live 声明，项目的平台 WIP / pre-alpha 边界不变。
- 首次本机 macOS（`9851dbe`）：Rust fmt / workspace Clippy 通过；workspace tests 440 过、9 忽略；规定的 Node 测试 361 过、1 跳过，TypeScript 通过。
  这些验证只覆盖协议元数据和回归检查，没有把它们当成真机远程操作或平台安全认证。
- PR #49 首轮 Windows native CI 的 11 项已记录断言均符合预期，但最后收集 fixture JSON 时失败；Windows fixture 每 100 ms 原位重写，
  最后的直接读取可能撞上写入空窗。保留首轮产物，未声称捕获了当时的空文件本体。`ui_fixture_state.py` 只有限重读新数据，
  不缓存旧状态、不重复 UI 动作；空/半写/UTF-8 截断后完成、持续损坏明确超时与不复用上次结果均有先红后绿的回归，native CI 同跑。
- 随后接到 main `1aefe9f`：完整保留 PR #47 的诊断章节、原生两轮采样和产物路径；仅在本节做并列记录。
  新基准本机 fmt / workspace Clippy、Rust 440 过 / 9 忽略、Node 374 过 / 1 跳过、TypeScript、fixture reader 4 项回归通过；新 head 的 CI 单独等待。

## 智能体工作区第二阶段 · 第一段：验收修复（2026-10-23，PR #45）

从 `origin/main` 的 `3e69f81` 接手，随后整合另一条线的 `a1efb9f`，在独立分支 `fix/workspace-acceptance` 推进。
初次**缺规格**停过，用户补交的 17 份 Markdown 与 2 张参考图已在仓库外就位；只按洁净室规格开发，没有读取官方应用实现。
规格、截图、运行日志和隔离认证均不进公开仓库；原工作区和其他会话的现场保持原样。

- **环境实测 / 基线**：macOS 27.0.1 arm64，外置盘起始可用约 1.6 TiB，Rust 1.95.0、Node.js 25.5.0、Python 3.14.2、CLT 可用，
  已完成 `npm ci`。初次 fmt、Clippy 通过，`cargo test --workspace --locked` 退出 101：IPC 旧实例退出后立即重绑偶发失败。
  所有失败与后续修复日志保留；不是全绿基线。16 GiB 内存的本机后续 Cargo 使用两项编译并发，复用一份 target。
- **历史 CI 查证**：`3e69f81` 与 #43 的 `4b73551`，两平台 test/native 成功。#43 的 iPhone
  [运行](https://github.com/JIA-ss/ai-remote/actions/runs/39929694837)被同一 main 后续 push 取代；`3e69f81` 的
  [运行](https://github.com/JIA-ss/ai-remote/actions/runs/39934119268)先发生 Codex 无障碍快照失败，再在缓存保存时撞 45 分钟上限。
  同步进来的 `a1efb9f` 已包含另线 iOS 修复及 CI 套件 9/9、折叠三连 3/3 证据；本轮没有在本机运行完整 Xcode/iOS 验收。
- **产品修复**：Claude 恢复共享文件预览与多文件 diff 入口、有效选区随滚动的引用、查找浮层、排队/未送达可见性及回放加载态；拒绝/停止的工具不再写成成功。
  Claude/Codex 首次 hello 前仅禁止提交、保留草稿；连接完成后的真实断线仍有未送达与重试。工作区 Codex 输入框增加底部留白。
  共享转录通知重置舞台浮层残留的定位属性，历史失败留在所属轮次，避免永久盖住后续终端；舞台提示定位不变。
  IPC 监听器关闭后显式释放锁，64 轮回归覆盖继承描述符仍存活、旧副本关闭不影响新实例互斥；原始偶发是否由 fork 触发未现场观测。
- **验收修复**：保留两条线的断言并集，适配原生直接会话栏，Watch/Back 核对同一会话 ID；旧 Steps 标签不能再作为测试开关。
  菜单首击失败仍失败，重试只继续采证；等待真实连接/回放、浏览器尺寸通知和原生截图确认，外部看门狗约束挂起，核对业务结果与应用退出码。
  Rust HOLD 替身只由下一条 user 消息完成，生成标题等控制请求不能当插话；原失败用例及旧/新协议探针通过。
  指针图片测试改用同一次原生快照，保留像素断言，避免把两次采样间允许的 None 误当失败。
- **本机原生条件实测**：每个临时 home 的 host/controller 四份配置均 quiet，非持久 WebView 存储，进程活动/后台节流仅对 debug smoke 生效。
  新日志抓到脚本外 `isTrusted=true` 激活导致切页；默认测试隔离外部指针/激活/按键，保留脚本合成操作及真实布局/滚动/控件通知，显式 visible 输入路径不变。
  几何探针发现 AppKit 会把有标题窗口约束回屏边；默认脚本只截图 WK 内容，因此仅该临时窗口移除原生 Titled 样式并移动 frame origin。
  harness 硬校验窗口与全部报告显示器均无相交，缺几何或仍在屏内立即失败。最终逻辑窗口实测 1280×800、DPR 2，真正离屏后 document.hidden，WK 截图正常。
  截图仅将白名单有限入场动画推进到终态，无限指示器保留；Claude 状态行按 TC-12 停留，截图等待真实终态，不改产品节拍。
  **未验 OS caption、物理键鼠、动画性能或帧率**；此前未核对实际几何的通过轮次不作为完全离屏证明，旧失败及旧图均保留。
- **最终门槛实测（代码基准 `efb0df4`）**：fmt、全工作区/all-targets Clippy、TypeScript、CLI 与 Tauri debug/no-bundle 构建通过。
  Rust 标准测试 439 过、9 忽略，另 13 项 mounts 集成检查通过；前端 368 项、367 过、1 个平台项跳过。
  CI 前端名单补齐预览、目录、输入及同步回归；同步回归共 91 项。
- **最终完全离屏 E2E 实测**：实际调试应用 → 控制端 → QUIC → 本机隔离被控端 → 替身，六套均连续两次通过：

  | 脚本 | 每轮通过项 | 连续轮次 |
  |---|---:|---:|
  | workspace | 110 | 2 |
  | Codex | 120 | 2 |
  | chat | 72 | 2 |
  | render | 12 | 2 |
  | offline | 7 | 2 |
  | preview | 31 | 2 |

  被控端替身 35 项连续两轮通过；真实 Codex 被控端 44 项通过（在先前本轮运行中验证，host 产品逻辑未变）。
  最终真实 Codex CLI 0.159.2、模型 `gpt-6-luna` 桌面整段 23 项通过，含真实 apply_patch、命令校验、改动卡、插话、停止、首击离开与按原 ID 重开回放。
  独立复核生成文件的 2026 年四个季度末日期。模型曾改用 Shell 写文件、没有编辑事件的负例保留，没有伪造编辑卡。
- **逐图复核**：当前完全离屏产物 132 张替身与 6 张真实 Codex，138/138 张逐张通过；实际文件集合无遗漏/重复，最终 SHA-256 与尺寸全部核对一致。只审测试会话栏或预览主体，不将截图当成物理输入或滚动交互证明。
- **未测 / 限制**：末次复核 Claude CLI 已安装但未登录，真实 Claude 的审批、问答、计划、标题和断线整段未测；本机 Windows/WebView2、
  完整 Xcode/iOS、真实键鼠/IME未测。macOS 结果不推导为 Windows 现场结果。早期未抓到输入来源的历史偶发不能全部归为本次外部输入。
  本轮不安装服务、不修改系统安全设置、真实 CLI 配置或既有应用；最终自有测试进程与临时认证配置均为 0。
- **装机状态**：另线安装记录留在其对应章节。本批代码基准 `efb0df4` 尚未装到 Mac 和 Windows 开发机；由两台开发机各自会话安装最终合入 main 的版本。
- **合入 / 下一步**：PR #45 在四项 test/native 必需检查成功后已进入 main `028ccc9`（2026-10-24，本地提交身份保持 JIA-ss）；
  独立 iPhone 检查当时仍在执行，随后运行 `39980248088` 已成功，PR #45 五项检查最终全部成功。main `028ccc9` 尚未装到 Mac / Windows 开发机。继续分别验收后两段；自建分组、
  拖拽、子会话树、深入搜索等继续列在 not_done，不冒充全部还原完成。

## 智能体工作区第二阶段 · 第二段：导航与 Claude 细节（2026-10-23 ~ 24）

本段已随 PR #47 合入 main `1aefe9f`，包含第一段的产品、完全离屏验收工具与最终文档。只依据仓库外洁净室规格，不读官方应用实现；
本段不改第一段已实测的历史数字，规格、原生截图、浏览器证据与认证资料继续留在公开仓库之外。

- **数字跳转（SB-41）**：所有平台用 Ctrl+Shift，按住 200 ms 给前九个实际渲染的会话行显示编号；物理 Digit / Numpad 键兼容
  Shift 后的标点。输入框、输入法合成、隐藏文档与错误修饰键不触发；释放修饰键、指针移动、滚动、失焦、可见性或缩放变化收起提示。
- **窄布局芯片（NS-34）**：按真实完整标签测宽，只把电脑和目录文字收成图标；名称、读屏文本与 tooltip 保留，权限、智能体和模型文字仍显示。
  ResizeObserver 处理尺寸变化，MutationObserver 处理模型等文字变化；恢复完整标签要有超过 2 px 的余量，避免临界宽度反复切换。
- **Claude 细节**：共享安全终端文本渲染处理 ANSI 16 / 256 / RGB、反显、CR / BS 覆写和不完整 ESC / OSC；成功 Bash 的无 ANSI
  diff 按 hunk 边界着色，错误输出不误当 diff。Claude Markdown 围栏按需启用 ANSI 并默认换行，复制仍保留原字节。
  提问题干、选项和说明支持行内 Markdown，但链接与文件名在按钮内不生成交互控件；选中文字后的 detail=1 激活不误选选项，
  detail=0 激活仍可选，问题与答案通过原协议原样回显。
- **浏览器已测**：数字跳转与芯片在 Camofox 前台演示后端下，深色中文 / 浅色英文各 25 项、各两轮通过，共 100 次断言、4 张图；Claude 隔离实际组件的浏览器检查
  30 项通过，深浅两主题共 12 张图已看过。这些结果分别是演示或组件证据，不当作真实被控端链路。
- **门槛实测**：全量 Node 381 项、380 过、1 个平台项跳过，TypeScript、fmt、全工作区/all-targets Clippy 通过；
  Rust 439 过、9 忽略，另 13 项 mounts 检查通过，Tauri debug/no-bundle 构建成功。产品代码基准 `903207c`，至验收脚本 `1b5a134`
  只变文档与测试，没有产品源码差异；CLI 沿用第一段相同代码和二进制，原生记录保存实际二进制与脚本哈希。
- **原生实测**：本机隔离被控端与控制端、四份 quiet 配置、实际窗口与所有报告显示器无交叠；工作区 110、对话 72、渲染 12、
  Claude 细节 28、导航/窄芯片 10 项，分别连续两轮通过。110/110 张本轮新 PNG 逐张通过，实际集合、SHA-256 和尺寸全部复验一致。
  Claude 细节覆盖真实替身协议的 ANSI/diff/复制原值、选字与激活、原题/答案回显；复制仅在本页截获并恢复，不写系统剪贴板。
- **新导航脚本**：`tools/desktop-workspace-navigation-e2e.js` 通过实际隔离被控端建会话；约束新建页容器 CSS 宽度，等待产品真实 RO / MO
  处理窄芯片、模型文字变化和恢复，不伪造 resize 通知或 `document.hidden`。本轮 WK 文档隐藏，数字跳转的隐藏/输入框禁止分支实测；
  正向数字跳转仍是上面的可见文档浏览器证据。合成 DOM 事件、DOM Range 和 detail=0 激活不冒称物理键鼠。
- **失败记录保留**：首轮 28 项仅计算样式断言失败；WebKit 的标准 `user-select` 不支持、前缀支持，三个目标的 `webkitUserSelect` 均为 `text`。
  编译 CSS 已有前缀，只修测试按 `CSS.supports` 选择计算字段并保留两字段日志，没有改产品 CSS。新增导航脚本两次过早读取异步 runtime/选择
  状态的失败也保留，现按实际 DOM 就绪等待，不加重试掩盖结果。睡屏时显示器枚举为空被硬门槛拒绝；临时防休眠进程恢复条件后重跑，未改系统设置。
- **仍未测 / 装机**：前台物理键盘数字跳转、IME/触屏/读屏器、真实 Claude 登录全流程，以及 Windows 的专用 details/navigation 脚本未测；
  P3 的 Windows 六套替身记录另列，不替代这两个专用脚本或开发机现场。产品基准 `903207c` 与脚本 `1b5a134` 没有由本会话安装到两台开发机。
- **后续**：P2 已合入；多选批量与扫光修复当时在 PR #48 继续验收，后续已合 main `35f54a3`，修复前后的数据与未测边界仍分别记录。

## 智能体工作区接进玻璃外壳（2026-10-23，main 的 PR #41 合进 `worktree-agent-workspace`）

main 新合入的玻璃外壳把设置、推流、配对都变成了一个窗口里的路由（`lib/nav.ts` + 历史），本分支的智能体工作区原来挂在旧
`App.tsx` 的 View 上，这次改成外壳的一个路由。设计见 `docs/agent-workspace.zh-CN.md` 2.1。
- **冲突取舍**：`App.tsx`、`ui/Shell.tsx` 以 main 为准再接回工作区；`index.css` 里 `glass/index.css` 照 main 的位置，`codex.css` /
  `workspace.css` / `claude.css` 放最后；CI 和上面"通用约定"的前端测试列表取并集，加上 `workspace` / `claude-view` / `chat-codex`。
- **路由**（纯函数，`tools/desktop-nav.test.mjs` 新增 7 项）：`Route` 加 `{ page: "agents"; host?; session?; place? }`；
  `parseRoute` 第三个参数是上次所在的半边，网址没有 `view` 时据此进工作区；`same` 按电脑 + 会话比（`place` 不算）；
  会话的 `parentOf` 是新建会话页；`hidden` 遇到"前一条是工作区"的推流就退回工作区并丢掉推流条目；`lastAgents` 给外壳侧栏的
  「智能体」用（回到历史里最近的工作区位置）；`modeOf` 决定每个路由属于哪半边，`App.tsx` 据此写 `ai-remote.mode`（设置、推流、欢迎页不改它）。
- **快捷键**：`KeyContext.agents` 为真时外壳不处理 ⌘N / 搜索 / 侧栏开关（工作区有自己的 ⌘N、⌘K、⌘B），历史、设置、⌘1–3 照旧。
- **组件**：`Workspace.tsx` 去掉自带的历史栈，‹ › 用外壳的 `back` / `forward`，打开页面走 `onRoute` → 外壳 `push`；新建会话页的预填从路由的
  `place` 读。外壳 `Sidebar` 加 `onMode`，渲染 `ModeSwitch`；位置与玻璃样式写在 `styles/workspace.css` 的 `.gl-side-modes`
  （展开时顶栏右端的胶囊，紧凑时竖排在顶栏下），没动 glass 包的文件。工作区整窗时不套 `.gl-win`，仍是自己的不透明面。
- **验证**（本机 macOS，camofox + vite 演示后端，端口 1451）：工作区 ⇄ 电脑往返、工作区会话 → 推流 → ‹ 回会话、‹ › 跨两半边、
  设置从工作区进出、外壳侧栏「智能体」回到上次的工作区位置、无 `view` 重开回到上次半边、工作区里 ⌘N 新建而不是配对、⌘\ 不改外壳侧栏偏好、
  鼠标侧键后退；12 张截图在 `/tmp/merge-glass-shots/`（不进仓库）。门槛：`cargo fmt --all --check`、`npx tsc --noEmit -p .`、
  `node --test tools/*.test.mjs`（248 项，246 过，2 跳过）。
- **没验证**：真实 Tauri 窗口（macOS 红绿灯与切换的间距、Windows 标题条）、`window-hidden` 事件本身（只有 `hidden` 的单测）、
  真正的窄窗（只用 ⌘\ 切到紧凑看过）。camofox（Firefox 内核）里 Codex 输入框有一条 2 px 的滚动条（textarea 内容高 30、框高 28），
  那是 `ui/chat/Composer.tsx` 的老样式，这次合并没碰。

## 桌面端毛玻璃重做 · 第一阶段：材质层与常驻外壳（2026-10-23，分支 `feat/desktop-glass`，worktree `../ai-remote-glass`）

用户反馈"进了设置只能点返回才能回主页"，要求整体 UI/UX 重做、所有界面主打毛玻璃、页面简洁现代。因为额度不够，
这次按"阶段性验收"只交付地基：材质层 + 常驻外壳导航。各页面的精修（第 2 段）没做，见"下一步"。
设计过程：4 路并行摸底（导航结构、样式层、测试依赖的选择器、玻璃平台调研）→ 3 份完整方案（Apple 式 / 简单优先 / 工程稳健）
→ 主控定稿（以 Apple 式为底，嫁接"会话里侧栏收成 72 px 图标栏"和"Pages.tsx 先拆文件"）。方案原文没进仓库（在本机 `/tmp/glass-design/`）。

### 决定

- **玻璃怎么做才是真的**：CSS `backdrop-filter` 只能模糊网页自己画的像素，永远采不到桌面，所以"全界面毛玻璃"=
  整窗一层原生材质（macOS NSVisualEffectView `sidebar`；Windows 11 Mica）+ `html/body/#root/.win` 透明 + 内容面画成
  半透明着色（磨砂由原生层完成，内容区不做 CSS 模糊）；只有浮在已绘制内容上的功能层（工具栏胶囊、菜单、对话框）
  用 CSS 玻璃 + 1 px 高光描边。**会话舞台（视频）上任何东西都不模糊**（5K@75fps 合成器本来就满）。
- `<html data-material="native|preview|none">`：App 内 = native（Windows 要 UA-CH 确认是 Windows 11，否则 none，避免 Win10
  透明窗口直接露出未模糊的桌面）；浏览器预览 = preview（画一张低饱和静态替身底）；设置里"减少透明度"、系统减少透明度 /
  增强对比 / 强制颜色 = none，回到原来 `native.css` 的不透明外观（逐像素对比 6 个页面，只有时钟文字不同）。
- 内容面透明度按 WCAG 公式推导：深色 `rgb(30 30 32/.76)`、浅色 `rgb(244 244 247/.74)`，卡片浅色 .78（**推导值，没在屏幕上量过**）。
- `tauri.conf.json` 的 `windowEffects.effects` 只留 `["sidebar"]`（Tauri 只取第一个，`windowBackground` 从来没生效）。
  钉住主题时调用 `setTheme`，让原生材质跟着变（新权限 `core:window:allow-set-theme`），不再把侧栏画成实色。没用 `liquidGlass*`
  （macOS 27 上打包版有 AppKit 崩溃报告，window-vibrancy#229）。
- **常驻外壳**：设置、会话、欢迎、配对都是外壳里的路由，侧栏一直在（只有沉浸全屏用 `:has()` 藏起来）；同一时刻只挂一个页面。
  `lib/nav.ts`（纯函数）：路由模型、全部旧深链（`?view/&sec/&tab/&host/&panel=chat/welcome/empty`）、Finder 式历史
  （‹ › 前进后退，没历史时回逻辑上级）、托盘意图（仍是"路由 + intent 同一次渲染"，HostPanel 照旧消费）、`window-hidden`。
  `lib/shortcuts.ts`：⌘[ ⌘] / Alt+←→、⌘,、⌘1-3、⌘N、⌘F、⌘\ / ⌃⌘S / Ctrl+\；输入法组字、对话框打开、远程画面上的按键一律不拦。
- **侧栏两种形态**：展开 240 px / 图标栏 72 px，按情境分别记忆（页面默认展开、会话默认图标栏）；开关固定在红绿灯右边。
  "你在操作"单独成组；离线组可折叠（≥3 台默认折叠）。
- **工具栏**：透明条 + 玻璃胶囊（‹ ›、本页唯一的 h1 + 副标题、中间槽、操作、唯一一个着色主按钮），不再放应用图标；
  设置的 6 个分区变成工具栏中间的分段胶囊（仍是 `nav.snav`），电脑页的标签也在工具栏。页面从工具栏下面滚过，滚动后才出现滚动边缘。
- 页面只做了接入外壳必需的改动：设备页去掉自己的大标题和第二个搜索框；电脑页顶栏并进工具栏、标签受控；活动页可按电脑筛选；
  欢迎页"设置这台电脑"直接打开应用内的开启对话框（不再给命令行）；找不到的电脑显示"找不到 {host}"。
- 文案：新键一次加齐在 `back:` 之后的锚定块里，并给后续工作包留了空块（`// glass: pages / computer / settings / overlays`）。
  改值 5 个：设置的"智能体"→"智能体接入"、"批准"→"批准方式"，电脑页标签→"编程智能体"，本机审计→"本机审计记录"，离线卡按钮→"返回"。
- `Pages.tsx` 拆成 `ui/pages/*.tsx`（纯搬家，`Pages.tsx` 只 re-export），后续各页可以并行改。
- **没动**：`Session.tsx`、`RemoteInput.tsx`、`native.css`、生成样式表、`src-tauri/src/**`、`bindings.ts`。新样式全在 `styles/glass/`
  （固定导入顺序 tokens → material → surfaces → shell → pages → settings → overlays → chat → stage → platform → fallbacks，
  其中 pages / settings / overlays / chat / stage 是空占位）。

### 踩过的坑

1. Camofox 是 Firefox 无头版，**不渲染 `backdrop-filter`**：浏览器截图里看不到模糊，只能核对计算样式；真效果要看真 App。
2. 滚动边缘层不显示时也挂着 `backdrop-filter` 等于每页顶部常驻一块模糊层，改成滚动后才模糊。
3. 对话错误条原来是红底红字（浅色主题下看不见字），旧问题，先在 `surfaces.css` 改成浅红底。
4. 侧栏开关不能用 `role=switch`：`network-ui-smoke` 要求文档里第一个 switch 是"局域网发现"。

### 实测

全部在本机 macOS 27.0.1，debug 包（`npx tauri build --debug --no-bundle`），HEAD = 本节第 5 个地基提交之后。
- 静态：`tsc --noEmit` 过；`npm run build` 过；`node --test tools/*.test.mjs` **143 项 141 过 0 失败 2 跳过**（基线 125/123/0/2，
  新增 `tools/desktop-nav.test.mjs` 18 项：全部深链、历史栈、托盘载荷、两平台快捷键表与守卫）；debug 构建后 `bindings.ts` 无变化。
  cargo fmt / clippy / test 没在本地跑（Rust 源码没改，只改了 `tauri.conf.json` 和 capabilities，debug 构建通过），交给 CI。
- e2e（本机隔离被控端，两端 `quiet = true`，替身智能体）：`desktop-chat-e2e.py` **67 项 ×3 次全过**；预览 e2e **31 项 ×2 次全过**；
  渲染 e2e **10 项 ×1 次全过**；离线 e2e **6 项 ×1 次全过**；`network-ui-smoke.py` **17 项 ×1 次全过**。
  对照：改动前（main 5d82098）同一台机器对话 e2e 67 项 ×1 次全过。
- 真 App（真实配置，连着 win-dev）：整屏截图裁窗 1 张（系统深色、一张风景壁纸）：侧栏透出原生磨砂，内容面半透明，没看到透明窗口里
  CSS 模糊取错色（tauri#12804）；真 WKWebView 里 ⌘2 → 待批准、⌘, → 设置（工具栏分区胶囊）生效。坑：osascript 的 `keystroke ","`
  在中文输入法下送不到，用 `key code 43 using command down`。
- **没测**：真 App 浅色外观、窗口失焦外观、会话窗口态 / 沉浸全屏真机清单、`stream-e2e.py` 帧率对比（舞台没加任何模糊、`Session.tsx`
  没改，但会话旁边现在多了 72 px 图标栏，未量）、Windows（Mica、标题条点击）、不同壁纸下的对比度。

### 没做 / 下一步

- [ ] 第 2 段页面精修（设计里拆好的工作包，文件已拆开可并行）：设备页去掉三块重复统计、卡片精简；电脑页 / 待批准 / 活动 / 配对 / 欢迎重排；
      设置各分区重排（`.hd` 大标题、"我"图块、版本行去重、快捷键分组）；`Modal.tsx` 统一对话框并 portal 到 body（目前对话框仍在页面树里，
      `data-modal` 还没有地方设置）；决策卡 Esc 只拒绝一张；会话舞台浮条的"舞台玻璃"外观（`stage.css`）；对话面板浮层（`chat.css`）。
- [ ] 真机验证：原生材质在不同壁纸下的对比度（内容面透明度是推导值）；tauri#12804（透明窗口里 CSS 模糊取到文字颜色）；
      滚动边缘上 mask + 模糊叠加；`setTheme` 后托盘菜单外观；Windows 11 Mica 是否透出来、plugin-frame 标题条是否挡住工具栏按钮（win-dev 未测）。
- [ ] `tools/desktop-ui-smoke.py` 已过期（找 `.capsule`、`.stream-request`），应改写成 Camofox REST 版的玻璃巡检（每页一个页面根、
      一个 h1、舞台内 0 个模糊元素、对话框不在 `.win` 里）。
- [ ] macOS 系统"减少透明度 / 增强对比度"在 WKWebView 里没有媒体查询，要 Rust 侧用 NSWorkspace 桥接（没做，目前只有应用内开关）。

## Codex 会话按 Codex 自家应用的样子显示（2026-10-22 ~ 23，分支 `feat/codex-chat-view`，worktree `../ai-remote-codex-view`）

用户要求：拆 ChatGPT 桌面应用（本机 `/Applications/ChatGPT.app`，包名 `com.openai.codex`，26.825，Electron）的 Codex 对话，
把它的对话和界面完整复刻到我们的 Codex 对话展示；Claude 和 Codex 分开实现、分开表现。

**怎么拆的**：`npx @electron/asar extract app.asar` 后对话界面就在 `webview/assets`（不像 Claude.app 那样从网上加载），没有
source map，用 prettier 展开后读：`split-items-into-render-groups`（一轮分成用户消息 / 工作条目 / 最终回答 / 改动 / 计划 / 审批）、
`agent-activity-units`（连续工具调用并成一组，单个且已结束的组退回成一行；组的摘要句按"改文件、读文件、跑命令、搜网页、调工具"
的顺序拼）、`conversation-blocks`（"Worked for 1m 23s"：最终回答一开始就把工作折起来，停止的轮次不折；插话消息折起后仍可见）、
`tool-activity-disclosure`（一行文字 60% 墨色，悬停变全色，箭头只在悬停时出现，展开动画）、`exec-shell-container`
（Shell 卡片：`$ 命令`、输出、右下角 Success / Exit code N）、样式变量（单元间距 16 px、组内 4 px、用户气泡 5% 墨色圆角 16）。
文案对照它的英文原文。**只借结构和视觉，不抄代码**（它是专有软件，我们是 AGPL）；拆出来的东西只在 `/tmp`，没进仓库。

**做了什么**：
- 被控端 Codex 适配器（`adapters/codex.rs`），全是可选字段 / 新事件，老被控端不发：
  - 命令的 `tool_call.input = {command, cwd, actions}`（去掉 `/bin/zsh -lc '…'` / `powershell.exe -Command …` 外壳的命令、
    Codex 自己对命令的解读：read / listFiles / search / unknown），标题改成 `Shell <命令>`；`tool_update.exit_code` / `duration_ms`。
  - 改文件的 `input.files`（逐文件 path、kind、move_path、统一 diff，新建文件的 diff 就是内容；整次调用共用 512 KiB、最多 1000 个
    文件，超出的文件 `diff` 为空、`truncated: true`，桌面端退回用调用的 content）；新事件 `turn_diff`（Codex 的 `turn/diff/updated`：
    整轮相对开始时的净改动，git 格式，512 KiB 截断，`turn_end` 之前发），改动卡片优先用它（同一文件改两次只算净变化，建了又删的
    文件不出现）。网页搜索不再带 `input.query`（标题已有查询词）。
  - `message.phase`（commentary / final_answer；结束时才说阶段的，补一条空 delta 带上阶段，不重发正文）。推理只转发摘要
    （`summaryTextDelta`），各段之间空一行；原始推理文本（`textDelta`）不转发，和 Codex 自家应用一样。
  - 提问：`question` 带 `request`（JSON-RPC 请求 id，一个请求里的题一起答）、`item_id`、`header`、`descriptions`（与 options 对齐）、
    `other`、`secret`、`blocking`；`prompt` 只放问题本身。同时可以有多个请求在等。仍用老的 `answer` 命令逐题回答，空串表示跳过，
    凑齐一个请求后只把非空答案回给 Codex（全跳过回 `{answers:{}}`）——**没加新命令**，因为 runner 进程会比被控端活得久，
    老 runner 不认识新命令会把回答丢掉。新事件 `question_done{request, answers, how}`：answered / skipped / resolved
    （Codex 发 `serverRequest/resolved`，或轮次结束时还没答）/ dismissed（轮次被停）。启动参数加了
    `-c features.default_mode_request_user_input=true`（0.137 和 0.150 都有这个开关；不认识的 `features.*` 键 Codex 也不报错，实测），
    让 Codex 在默认模式下也能提问。**默认模式的提问不阻塞**（`isBlocking: false`）：Codex 自家应用约 1.5~2.5 分钟没人答就用空答案放行，
    模型自己拿主意；适配器照做，`QUESTION_WAIT` 120 秒后回 `{answers:{}}`、发 `question_done{how:"expired"}`，提问事件带
    `expires_in_ms`，卡片上显示倒计时。不这样做的话命令行（`chat run` / `send --wait` 没法作答）发起的任务会一直挂着。
    会话层的"30 分钟没有事件就停止"看门狗在有提问等着时不计时。
  - 提问的 `request` 名是"进程启动毫秒-JSON-RPC id"：Codex 每个进程的服务端请求 id 都从 0 开始，而一个会话会跨多个进程
    （空闲 20 分钟收进程、`thread/resume`），只用 id 会让新提问和老提问撞名。回复 Codex 时仍用原始 id。
  - 对话 hello 的 `runtime` 多了 `protocol`；轮次自己的错误信息原来把服务商 JSON 原样显示，改成取里面的 message。
  - **老控制端**：`turn_diff`、`question_done`、`phase` 忽略；提问显示时少了 header；只有移动没改内容的编辑没有 content，老控制端
    会把 `input.files` 当 JSON 显示一块。
- 桌面端（`lib/codex.ts` 纯函数投影；`ui/codex/` 组件；`styles/codex.css` 全部限定在 `.cdx` 里，类名加 `cdx-` / `x-` 前缀——
  `.live`、`.hd`、`.row` 是全局类，第一次截图就被 `v2.css` 串了样式）。`AgentChat` 按 `runtime.protocol`（老被控端退回看 id /
  名字）给 Codex 会话加 `.cdx` 并换 `Transcript` 的 `variant="codex"`；Claude Code 和 ACP 智能体照旧走 Orca 那套行。按 Codex 的样子：
  - 一轮：用户气泡（70% 宽、圆角 22）→ 出现第一个工作项后顶部一条"处理中 / 已处理 12秒"计时分隔（答案一开始就定格成"用时 X"并成为
    折叠开关；停止写"你在 X 后停止了"）→ 工作（连续工具调用并成一组：进行中组头写当前在做什么，至少停留 1 秒；完成后写
    "工具→编辑→读取→运行→网页"顺序的一句摘要，前面一个图标；单个已完成的组退回成一行）→ 轮末一行状态（推理标题，没有就
    "思考中"；有待答提问时"等你回答"）→ 答案（一开始就把工作折起来，停止的轮次不折，插话折起后仍可见）→ 改动卡片 → 复制和时间。
  - 推理不成行，只把摘要最后一行（整行加粗取粗体里的字）当状态文字。计划不进对话，在输入框上方显示成"第 2 / 5 步"胶囊（环形进度，
    悬停列出全部步骤）。提问在输入框位置换成卡片（输入框保持挂载，草稿不丢）：一次一题、`1 / 2` 翻页、编号选项（默认选第一个）带说明和
    "推荐"徽标、"其他"行、跳过 / 下一步、数字键和方向键、X 或 Esc 关闭即停止本轮；答完后流里留一行"已询问 N 个问题"可展开看答案。
  - 进行中的文字用节拍闪烁（出现 600 ms 后扫一次，之后每 4 秒一次；扫光层只在扫的那 1 秒存在，查找和复制只看到一份文字）；
    子智能体行用连续闪烁。
  - Shell 卡片：命令最多两行点开看全；输出 11px，盒子 144 px 高、用 `column-reverse` 不靠脚本就钉在底部、长行横向滚、可滚方向有渐隐，
    不再截 40 行；ANSI 颜色（16 / 256 / 真彩、`\r` 覆盖、`\b` 退格）；页脚统一灰色，Codex 没说退出码时写"退出码未知"。
  - 改动卡片："已编辑 3 个文件" + 统计 + 审阅（一次展开全部 diff）；多文件先列 3 个，新建 / 删除文件一个小圆点，⌘ 点击打开预览。
    diff 带行号，"复制补丁"只给 `git apply` 能用的补丁（老被控端的 diff 没有行号就不给）。
  - 输入框只一个圆按钮：运行中且没输入是停止，否则是发送 / 插话 / 排队；连按两次 Esc（2 秒内）停止，第一次按下按钮显示"Esc"；
    答案已经在流的时候发送改为排队；⌘/Ctrl+Enter 排队，⌥Enter 换行。
  - 列表跟随：读者任何向上的动作（滚轮、拖、方向键）立刻放手，只在离底 24 px 内且 1 秒内没有向上动作时才重新跟随；回到底部的
    圆按钮 260 ms 动画，运行中按钮里三个点在动。回放追平之前显示"正在加载对话…"（两种视图都是），不再闪一下"空对话"。
  - 字号颜色：Codex 的种子色（墨色、强调色、增删色，深浅两套），正文 14px、行高 22.75px，段距按字号四分之一的倍数。
  - 时长两种格式（Intl 单位格式，跟随界面语言）：命令行用"12s / 1m 0s / 1h 0m 3s"，计时分隔用"0s / 1m / 1h 5s"。
- iOS（`apps/ios/Sources/PhoneCodex*.swift`，分支 `feat/ios-codex-v2` 于 10-23 合进来）跟上了桌面端 060cabd 的样子：推理只作
  轮末标题、计时分隔"处理中 / 用时"、计划胶囊、提问卡（一次一题、编号选项带说明和推荐标、自填答案、跳过 / 下一题、X 停止本轮、
  非阻塞提问的倒计时，每题一条 `answer` 命令）、"已询问 N 个问题"一行、改动卡"已编辑 N 个文件"+ 审阅、Shell 输出保留终端颜色；
  `lib/codex.ts` 的时长、计划步、网页抓取、组摘要、按轮次的请求、合并编辑都照搬并移植了单测。提问结束后的那一行按 `how`
  说"已回答：… / 已跳过 / 未回答，Codex 已自行继续 / 已停止"（被控端回来的答案为准）。**老版桌面端**不认
  `question_done`：Codex 的非阻塞提问被被控端 120 秒后放行以后，它仍显示提问，这时再答会收到一条 `late_answer` 提醒（"回答到达时
  Codex 已经自己继续"），答案不会送出。
- 演示后端（`lib/demo.ts`）有一个 Codex 会话和一段 Codex 形状的实时回复，`?view=session&host=office-pc&panel=chat` 打开后在
  会话列表里选 Codex。

**测试与实测**（2026-10-23，本机 macOS，debug 包）：
- 单测：`tools/chat-codex.test.mjs` 37 项（`git apply --check` 验证复制出的补丁、ANSI、时长、提问分组等）；`cargo test -p ar-host --lib chat`
  73 项（Windows 上同一命令 58 项，部分测试只在 Unix 编译）；`cargo test --workspace` 420 项、`clippy -D warnings`、全部前端测试过。
- `tools/fake-codex-app-server.py`：Codex app-server 的 JSON-RPC 替身（走真实的被控端适配器和 runner），按关键词演一轮：EXPLORE、
  ASK（一个请求两道题）、RESOLVED（Codex 自己关掉提问）、STEERME、SLOW（收到中断后 0.8 秒才停，像真 Codex）、FAIL、MANY、NOPHASE、
  COMPACT、REEDIT（同一文件改两次 + 建了又删，发 `turn/diff/updated`）、BADPATCH、LATEPHASE、DRAWL（慢慢流 8 秒的答案）。
- `E2E_SCRIPT=tools/desktop-chat-codex-e2e.js python3 tools/desktop-chat-e2e.py`：真实桌面应用 + 隔离被控端 + 替身，111 项全过
  （104 项那一版在 Mac 连续 2 次、win-dev 上 3 次全过；109 项那一版 win-dev 上也全过）。这一轮抓到并修掉的真问题：停止时"正在停止…"一闪而过（替身停得太快，两条事件同批到达——替身改成像真 Codex 那样稍等）、
  "审阅"收起时收不回单独点开的文件、提问卡首屏依赖淡入动画（后台窗口里动画可能不走，改成只在翻题时淡入）、引用按钮被页面自己的
  平滑滚动清掉（改成跟着选区移动，Claude 视图同样受益）。
- 回归：Claude 视图的 `desktop-chat-e2e.js` 多次全过；最后一轮 3 次里有 1 次第一项"记住的会话已不存在时回到会话列表并提示"
  超时（面板回到了新会话页，但没有提示；判定在 `useConversation` 的连接错误分类里，这次没改那部分），另外 2 次全过，判为偶发。
- 真实 Codex：`E2E_MODEL=gpt-5.6-luna E2E_CODEX=/Applications/ChatGPT.app/Contents/Resources/codex E2E_SCRIPT=tools/desktop-chat-codex-real.js
  python3 tools/desktop-chat-e2e.py --real "Codex (path)"`（codex-cli 0.150.0-alpha）全过：读文件、改 export.py、跑校验命令、折叠与改动
  卡片、插话被采纳、停止 `sleep 120`、重开回放。**坑**：本机 `~/.codex/config.toml` 的默认模型 `gpt-5.5` 在 ChatGPT 账号下 0.137 和
  0.150 都报"不支持"，要在开始表单的模型里填账号能用的（`~/.codex/models_cache.json` 里列着）。
- e2e 偶发点（已处理）：60 ms 轮询在后台窗口里赶不上不到 1 秒的流式窗口，相关检查改用 MutationObserver。
- **没查清的偶发**：点 ⋯ →"其他会话"偶尔没有离开当前会话（面板还停在对话里，菜单已关）。替身 e2e 里见过 1 次（关查找栏之后），
  真实 Codex e2e 里 1 次（停止一轮之后）；之后写了专门的脚本反复"离开 → 重开"28 次（空闲、刚停止、刚关查找栏、刚截图）都没复现。
  两个 e2e 脚本在这一步都加了一次重试；原因待查（怀疑点击时菜单元素被重新渲染，点到了旧节点）。
  **2026-10-26 查清**：落到设备页那次是测试以外的真实输入点进了实际在屏幕上的调试窗口，不是产品问题，见"两台开发机装上 main `ccbcf37`"一节。
- 合并前做了一轮对抗式审查（3 个方向各自找问题、每条再由独立 agent 试图推翻），确认 16 项，全部修掉并补了单测或 e2e：
  非阻塞提问没人放行（见上）；请求 id 跨进程撞名导致新提问卡不出现、旧答案被改写（`question_done` 现在只改本轮，草稿和"已关闭"
  按轮次记）；正在输入框打字时卡片抢焦点、下一个数字键 / 回车替用户作答（卡片首次渲染时记下焦点，在输入框里就不抢；输入框被隐藏
  会把焦点挪走，挂载后再看已经晚了）；焦点在面板任意按钮上按回车会被卡片当作作答（现在只从对话列表本身转发按键）；关卡片后焦点
  掉到 body；上下文弹层开着时 Esc 被当作停止的第一下；`\r\n` 行尾丢掉颜色、退格和擦行不认转义、半截转义显示成乱码；
  `curl … | sh` 之类被算成"搜索网页"；没有整轮 diff 时"新建再修改"给出 git 不认的补丁；答案开始后再停止，时长只算到答案开始；
  Codex 视图的列表尺寸观察在 hello 之前挂载时取不到 Codex 标记。
- 对这批修复再审查一轮，又确认 4 项（都已修，附测试）：iOS / 老桌面端不认 `question_done`，过期后仍显示提问且之后的回答被悄悄丢掉
  （iOS 补上处理，迟到的回答发 `late_answer` 提醒）；截止前一刻的回答可能输给自己的截止时间（`select!` 改成 `biased`，命令优先）；
  读者往上翻、内容继续流入后列表变矮，视图会往前跳（改成按容器高度差移动）；⋯ 菜单没有 Esc / 点外面关闭，开着时 Esc 停不了。
- 第三轮审查（合进 main 之后，分支 `feat/codex-followups`）又修 3 项：iOS 提问结束一律显示"已回答"（改成按 `how`）；新请求里
  有同名问题时没从"已关闭"列表里去掉，回答被误判成迟到（`late_answer` 误报）；列表变高 / 变矮时按读者自己的滚动位置
  （`readerTop`）算，浏览器在观察回调之前就把 `scrollTop` 夹过了。之后新加的 e2e 检查"列表变高、离底很近时也保持位置"仍然
  每次都差（视图跟着流式内容走了 91 px）：浏览器夹 `scrollTop` 时派发的 scroll 事件比 ResizeObserver 先到，处理函数看到
  "已在底部"就重新跟随。现在列表高度变了而 ResizeObserver 还没处理时，忽略这次滚动（`listHeight`），改后 58 对 58。
- 10-23 合进 main 前（含 #43 智能体工作区）复跑：Codex e2e 第一轮 3 项失败——上面这项、"⋯ → 其他会话"后落到设备页（上面"没查清的偶发"的新表现，
  第二次起没再出现）、Claude 会话那项的旧选择器（#43 把 Claude 会话换成 Claude 式视图 `cl-root`，`.nt-turn` / `.nt-run` 没了，检查已改）；
  修完后 **112/112 全过（1 次）**。前端单测 258 项（256 过 2 跳过），`cargo test -p ar-host --lib chat` 90 项，iOS 模型测试全过。
- iOS UI 测试（本机 iOS 26.2 模拟器，Codex 4 项 + Claude 视图 3 项）：5 过 2 失败。
  - `PhoneChatUITests.testPreviewsComposerMenusImagesDeliveryProjectsAndFolders`"链接打开页面预览"：iOS 移植之前（a89af8b）
    同样失败，不是这批改动带来的，原因没查。
  - `PhoneCodexUITests.testCodexTurnFoldsAndOpensToGroupsShellsAndDiffs`：**iOS 移植带进来的**（移植前 32 秒通过）。点开改动卡里的
    文件后，XCUITest 查询 `codex-changes` 下的 `codex-diff` 超时（2 分钟）；那时应用主线程 100% CPU，采样全在 SwiftUI
    `LazyStack` 反复布局里，没有我们自己的代码帧。手动走同样步骤 CPU 一直是 0，所以是**抓无障碍快照时**才进入布局死循环——
    VoiceOver 用户可能也会碰到，要当真 bug 查。下一步：二分 `PhoneCodexRows.swift` 移植后的改动（`.accessibilityElement`
    容器、`onAppear` 改状态、`TimelineView`、Shell 输出框的 `defaultScrollAnchor`），看哪一处让快照时布局不收敛。

**收尾复核（2026-10-23，基于 main 3e69f81）**：
- 用户追加的界面要求已实现：远程桌面右侧直接复用现有 Claude/Codex 会话组件，去掉“步骤 / 对话 / 智能体视角”、步骤时间线和旧留言输入。
  待审批卡独立显示在会话上方；Esc 仅在卡内拒绝，避免聊天关查找/菜单时误拒审批。接管后的观看开关与交还说明放在舞台设置面板，非模态 region 不拦聊天 Esc。
  工作区“看电脑”携带当前会话 ID，返回时仍是同一会话，路由不复用一次性票据。Camofox headless 演示页面已核对并保存截图。
- 指定提交的 CI：Desktop and core（39934119396，两平台）与 Native structured UI（39934119355）成功；iPhone controller
  （39934119268）总状态 cancelled，但测试先在原折叠/改动卡快照处失败（136.670 秒），其他三项 Codex 通过。原预览用例当轮通过（77.316 秒）。
- iOS 诊断保留了失败：同轮未改基线 145.797 秒失败；单独移除裁剪先通过（80.781 秒），后重复第二遍仍失败（134.696 秒）。
  底部锚点组合也未解决；非惰性诊断版本连续 3 遍通过（71.321 / 71.015 / 58.581 秒）。最终仅 Codex 改用 VStack，首次挂载最近 30 轮，
  按会话每次展开更早 30 轮，模型仍保留原来的 200 轮上限，Claude 仍用 LazyVStack。新 35 轮 UI 用例核对数量、顺序、无重复与首轮可见。
  diff 每行补完整无障碍标签（增删、旧/新行号、代码），保留容器语义。单文件展开和“审阅”全展开分别做 elementDetection 审计，不过滤问题。
  本机只有 CLT，完整 iOS 验收在 Xcode 26.2 的 CI Mac 按明确模拟器 UDID 运行；没有声称做过真机 VoiceOver/Inspector 手动遍历。
- 最后诊断运行 39954997454：Codex 五项全过，原折叠用例含审计 62.681 秒、分页 55.539 秒；独立重复折叠 3/3 通过。
  仅旧预览用例失败。原始 app 日志显示 WebContent 冷启动耗时 34.779 秒，旧正文检查只等 5 秒，检查结束时进程尚未启动；
  不能据空白末帧推断导航竞态。测试现等待正文最多 45 秒（配置预算，不是实测耗时），强制验证链接、正文和脚本禁用；新增卡片→链接→卡片
  三次打开、每次源码→预览重建，共 6 次 HTML 加载。产品预览 Coordinator 未改。最终运行 39959115495：九项套件 9/9 通过一轮，原折叠/审计 66.817 秒、分页 45.944 秒、
  旧预览 68.525 秒、新六次加载 49.267 秒；独立折叠三连 3/3（84.016 / 64.645 / 64.805 秒），不是失败重试计绿。
  iPhone workflow 总限时由 45 改为 60 分钟，为 SDK 构建、归档、UI 用例与缓存留空间，不放宽断言。
- 桌面端补齐本轮发现的 Claude 引用滚动跟随和 hello/回放加载门，选区取消后才清掉引用；行为单测先复现再修复。
  仅调试 smoke 使用非持久 WebView 存储、进程活动防止 App Nap，显式可见模式请求焦点；Release 与全局偏好不变。
  harness 退出或中断只清理自己的隔离实例，保存完整 stderr 与去标识的可见性信息；早期锁屏/遮挡失败和中断不计通过。
- Mac 原生 CI 验收 39953111022 一轮全过：Claude 默认脚本 69 项、Codex 替身 114 项、工作区 106 项，含“看电脑”右侧同一会话和返回。
  本机 Claude 默认脚本完整 69 项成功一轮；最终 Codex 替身在重建后的可见 debug 包 116/116 成功一轮（新增两项区分退出会话与表单就绪）。驱动适配英文文案、减少动画设置和后台 20 秒自然轮询；工作区第二连接只响应 hello.seq 之后的新审批，
  避免重放旧事件时误答历史 rid；未以强刷替代自然更新。本机最终工作区 106/106 一轮全过（149.4 秒，可见但未聚焦）。
  上一轮四项失败中，三项源于测试未等连接完成便发消息（截图明确未送达），一项源于乐观移行后立即检查保存完成提示；
  驱动现等待连接/回放就绪与真实 toast，不放宽业务断言。实测 FAIL 发送后 156 毫秒后端已记 failed，自然侧栏 2.842 秒变红。
- 真实 Codex 本机原生 E2E 22 项全过一轮：可见调试窗口、隔离 quiet 被控端/控制端，Codex 0.159.2、ChatGPT 已登录、gpt-5.6-luna，
  脚本约 49 秒；读/改 export.py、运行校验、diff、插话、停止 sleep 120、重开回放，“其他会话”首点成功。登录信息未传到 CI。
- “其他会话”新捕获的一次现场实际已退出对话、列表已有记录，只是 runtimes 表单未就绪。测试先确认退出，再独立等表单，不重点击菜单；
  8 个 VM 行为用例覆盖延迟、首点真失败和错误路由。首点真失败仍记失败；旧“落到设备页”的产品原因本轮未复现，继续保留待查。
- 本机最终检查：前端 14 文件 267 项，266 过、1 跳过；TypeScript 通过；iOS 纯模型 79/79（CLT Swift 6.4）；Rust fmt/clippy 通过、全量 438 过、0 失败、9 忽略。
- Mac 完整 Release 已安装到 `/Applications/ai-remote.app`，含桌面端、CLI、hostw；本机临时签名，deep/strict 验证通过，三份可执行文件 SHA-256 与构建包一致。
  目标目录原本不存在且当前用户可写，临时目录校验后排他原子重命名安装，无需提权。Finder 已确认应用文件。旧用户级后台被控端仍从原目录运行，
  后台注册迁移和系统授权继续走 App 入口。待办：app 本体移到应用程序目录的一键入口、完整卸载入口；不要求人在终端输入提权命令。
- 按用户最新范围，仅做本机与 Mac CI 验收；另一平台的现场测试、桌面安装和服务替换未执行。

**没做 / 没验证**（按收益排序，规格里的 Should / Later）：
- 输入框整体外形（两行布局、"完全访问"标签、上下文环弹层、队列托盘、读图等待、历史）、头部（文件夹按钮 + 标题 + ⋯ 菜单、连接横幅、
  hello 之前就用 Codex 外观）、Codex 重试提示、撤销 / 重新应用（需要被控端 `git apply` 命令）、编辑最后一条消息（`thread/rollback`）、
  代码块换行开关、用户长消息折叠、时间分隔条、复制为 Markdown。
- 审批卡片（我们 Codex 跑在免审批模式）、分叉、子智能体面板、MCP 应用嵌入、图片生成预览没有对应功能，没做。
- Windows 上真实 Codex 的 `command` 字段长什么样没实测，只按 shlex 拼接推断并写了单测。

## 合并进 main：提交身份统一与一次集成（2026-10-22，分支 `integrate/2026-10-22`）

**提交身份**：仓库里只用一个身份 `JIA-ss <627723154@qq.com>`（作者和提交者都是）。2026-10-22 发现 10-20 起的提交带着
机器全局 gitconfig 里的工作身份，于是改写了全部历史：树和时间不变，`Co-Authored-By: Claude` 尾注去掉，提交说明里引用的
哈希换成新的；GitHub 上全部分支强推，Mac 和 Windows 开发机的本地分支原子地挪到新历史，旧对象已清掉。
- 两台机器的 `.git/config` 都设了 `user.name` / `user.email`（全局配置不动）。提交前 `git var GIT_AUTHOR_IDENT` 必须是上面的身份。
- **不要再用 2026-10-22 15:20 之前的提交哈希**（旧哈希在本仓库已不存在）；不要把改写前的提交推回来。
- 在 GitHub 网页上合并 PR 会产生提交者为 GitHub 的合并提交，作者是账号的默认提交邮箱：要保持单一身份就在本地合并后推送，
  或者确认网页合并用的邮箱。
- 已合并 PR（#1~#19）的页面上还看得到改写前的提交（GitHub 的 `refs/pull` 保留着），只有 GitHub Support 能删。

**这次合进 main 的**：PR #16（macOS 登录窗口交接）、#14（托盘与"本设备"页）、#23（iPhone 网络设置）、
`fix/fullscreen-controls`（全屏接管时唤不出菜单，原来是主检出里没提交的改动）、#20（被控端编程智能体）及其上的
`claude/chat-input-8764a1`（输入侧补齐）、#22（iPhone 对话）、#21（服务模式下用户会话里的 chat-runner）。
冲突：`AGENTS.md`（各自新增的章节，全部保留）、`PhonePairing.swift`（#16 的输入框焦点和 #23 的网络页开关并存）、
`PhoneRootView.swift`（网络页和对话页两个 sheet 并存）、#21 与 `feat/host-chat` 的 8 个文件。
**集成时补的**（各自分支上都没有，合在一起才需要）：
- `control.rs`：#14 要求每个控制请求都在 `HostRequest::OPS` / `op()` 里有线名（`status` 报给桌面端），#20 新增的 `chat`、
  `set_chat_enabled` 没登记，合起来编不过。
- #21 的 runner 是照 51 个提交之前的 `feat/host-chat` 写的：管道两端补了图片（prompt 带 `images`）和插话（`steer` 命令）；
  `NOTICE_WORDS` 补上 `compacted` / `model` / `no_images` / `other` / `steer_queued`（runner 传回的通知码不在表里会变成
  `other`）；"能不能收图片 / 插话"改由 `runtimes::caps_of` 从描述表得出，不用找程序，runner 模式和接回的智能体都适用。
- **安全**：`@` 文件列表原来在会话目录里跑 `git ls-files`。服务模式的被控端是 SYSTEM，而仓库配置（例如 `core.fsmonitor`）
  能让 git 执行程序，等于让用户以 SYSTEM 身份执行任意程序。runner 模式下改为只遍历目录（`FileCache::new(false)`）。
- runner 测试里的替身 Claude 把 `initialize` 当成一轮，第一轮提前结束；改为应答 `control_response`，并新增
  `an_image_crosses_the_pipe_to_an_agent_in_the_runner`。

**没合的**：`claude/eloquent-torvalds-a5b249`（iPhone 对话跟进桌面端，和 #22 的 `ccc95de` / `531eaea` 是同一件事的两次实现，
而且还带着用户已经否掉的插话芯片；以 #22 为准，分支留在本地）、`claude/agy-adapter-parked`（Antigravity 按用户意思暂不接）。
`test/*`、`tmp/*`、`backup/desktop-chat-e2e-6cb5281` 是一次性的测试 / 打包 / 备份分支，内容已在别处，已删除。
`CLAUDE.md` 和 `docs/research-host-agent-chat.zh-CN.md` 留在本地不入库（后者含本机路径）。

**实测（本机 macOS，集成分支）**：`cargo fmt --check` 过；`cargo clippy --workspace --all-targets --locked -D warnings` 无警告；
`cargo test --workspace --locked` 397 过 0 失败 8 忽略；`cargo test -p ar-host --lib chat` 58 过（含新增的 runner 图片测试）；
`node --test tools/*.test.mjs` 115 项 113 过 2 跳过；`tsc --noEmit` 过；`tools/desktop-chat-e2e.py`（debug 桌面包 + 本机隔离被控端，替身智能体）67 项全过（1 次）。CI（macos-15 / windows-2025）见 PR。
**未测**：#16 的 7 个真机交接场景（要人在这台 Mac 上操作）；#21 服务模式真机（步骤见"服务模式：用户会话里的 chat-runner"），
经 runner 的插话、图片、重启续接后来补了测试，在 Mac（Unix 套接字）和 Windows（命名管道）上都过了，见该节；全屏修复在真实 App
原生全屏和 Windows 控制端；#23 真机 iPhone；#14 Windows 干净机器安装。
（2026-10-22 补测：~~chat-input 在 Windows WebView2 下的输入界面~~、图片走 Windows 命名管道，见下一节。）

## Windows 补测：新输入框在 WebView2 下、图片走命名管道（2026-10-22，分支 `test/win-chat-input`，基于 main 8eeabb6）

集成说明里标为未测的两件，在 win-dev（Windows 11，5120×2160，WebView2，debug 构建，独立检出 `%USERPROFILE%\ai-remote-wininput`）上补测。
测试用的被控端、控制端都是隔离目录、安静网络模式；真实被控端（服务和 launchd）没碰。

**新输入框（WebView2）**：`tools/desktop-chat-e2e.py` 经 zremote 在交互会话里跑（窗口在屏幕外，PrintWindow 截图）。
- 替身智能体 67 项 ×3 次全过：一行输入框（+ / 文字 / 发送）、`/` 弹出（说明、技能分组、Tab 选中）、`@` 弹出（被控端列出
  `src/main.rs`，Windows 上也是正斜杠）、粘贴图片（WebView2 接受构造的 paste 事件）、发出后气泡缩略图且替身收到图片、送达后没有
  残留的"发送中"、子智能体、⌘F 查找（Highlight API 当前项 / 其余项都画出来了）、引用、用量明细、插话、排队、回放。
- `tools/desktop-chat-e2e-render.js` 10 项 ×1 次全过：输入法组字时回车不发送、普通回车发送（**合成的** `isComposing` / keyCode 229
  事件，没有用真实中文输入法打字）、长消息折叠、`content-visibility`、字体。
- 真实 Claude Code 2.1.293（`claude.cmd`，已登录）：`desktop-chat-e2e-real.js` 扩了 `/` 菜单里的项目命令（工作目录
  `.claude/commands/pong.md`，Tab 选中后发送，回 SKILLPONG）和粘贴一张红色 PNG（回"红色"）。第 2 次 21 项全过；第 1 次只有
  "认出图片"一项失败，原因是 Claude 按机器上的全局设置用中文答了"红色"，断言改成 `/red|红/`。插话 2~3 s 进同一轮，数到 5 停下。
- 截图在 Mac 的 `/tmp/win-chat-input/`（只裁了右侧面板的副本叫 `panel-*.png`；整图里有被控端桌面，不进仓库），看过。
- 测试脚本的修正（产品代码没改）：Windows 上图标是 Segoe Fluent 字体里的私用区字符（例如 U+E76C 右箭头）作为文字渲染，
  行的 `textContent.trim()` 去不掉，文件夹行改用 `.fn` 的文本比较（`desktop-chat-e2e.js`、`desktop-chat-preview-e2e.js`）；
  `CFG.work` 按 `\` 和 `/` 都拆；`desktop-chat-e2e-render.js` 还在用文件夹选择器之前的 `#ag-cwd` 文本框，改为走选择器。
- **未测**：真实中文输入法；拖文件进输入框、点 + 选文件（只测了粘贴）；高 DPI 缩放；浅色主题。e2e 期间被控端推流有一次
  `hardware encoding failed; using OpenH264`（同机 zremote 也在用 NVENC，没查），和对话无关。

**图片走 Windows 命名管道**（`chore/runner-verify`〔PR #27〕同一天也做了同样的移植，合 main 时以它的版本为准，它多了经管道插话的用例）：`chat/runner/tests.rs` 的 `restart` 模块原来只在 Unix 编译（替身是 sh 脚本），现在 Windows 上替身是
PowerShell 脚本（`powershell.exe -File`，不经 cmd.exe），端点是 `\\.\pipe\ai-remote-chat-runner-test-<临时目录名>`，经同一套
`ar_net::ipc` 和 `ChatManager` 的 runner 后端。win-dev 上 `cargo test -p ar-host --lib chat::runner` 7 项 ×3 次全过，其中
`an_image_crosses_the_pipe_to_an_agent_in_the_runner`（图片经命名管道到智能体）和
`an_agent_outlives_its_host_and_its_session_goes_on`（被控端中途死掉、再接回同一个智能体进程）是第一次在 Windows 上跑。
这走的是 runner 和被控端在**同一进程、同一用户**的测试路径；SYSTEM 被控端以 `CreateProcessAsUserW` 拉起用户会话里的 runner 那一段
当时没做；同一天用户授权更新 win-dev 的真实服务后，步骤 1~2 有了真机结果（见下）。

**win-dev 真实服务更新（2026-10-22，用户授权）**：服务 `ai-remote-host-cc166525` 原来跑 `feat/lan-direct` 检出的 release 包，
没有编程智能体（控制端报 `unknown tool 'chat_runtimes'`）。用本分支（main 8eeabb6 + `which` 修复）出 release 包，复制到
`%USERPROFILE%\ai-remote-svc-bin`（不指向构建目录，免得重编时覆盖正在跑的程序），经 zremote 提权会话 `host install --service`
重装、`sc start` 启动。结果：被控端和监护进程是 SYSTEM，`--chat-runner` 进程是登录用户（`CreateProcessAsUserW` 拉起，真机第一次）；
`doctor` 全 ok（安静模式、仅局域网 `:47800`、chat）；Mac 控制端 `ai-remote call --host win-dev chat_runtimes` 返回 `can_start: true`，
Claude Code 2.1.293（已登录，`D:\nodejs\node_global\claude.cmd`，用户 PATH 里找到的）、Codex 0.162.1 等。
服务模式步骤 3~8（真实对话、锁屏续接、stop-host、安全检查、卸载）仍未做。

**接回时丢请求（真实竞态，CI 撞出来的）**：PR 的 macOS CI 上 `an_agent_outlives_its_host_and_its_session_goes_on` 失败一次，
本机同一测试 10 次失败 2 次（这条 Unix 路径本分支没改，是原有问题）。加日志后不再复现，改用只记阶段的探针才抓到：
重启后的被控端发出了 `lives` 请求，runner 一侧从没收到。原因是被控端握手最后写 `{"ok":true}`，接回流程紧接着就发 `lives`，
runner 握手用的 `BufReader` 一次读进了两行，而 `serve` 里 `wire.io.into_inner()` 把缓冲连同 `lives` 一起丢了，被控端干等
60 s（`LOOKUP`）。真实服务模式下被控端重启（例如锁屏切会话）后接回智能体，走的正是这一步。修法：两侧 `serve` 都保留握手的
`BufReader`。新单测 `a_request_right_after_the_handshake_is_answered`：没有修复时 3/3 失败，修复后通过；接回测试修复后本机 40/40 过。
注意：win-dev 真实服务跑的是修复前的包（见上），服务模式步骤 4（锁屏续接）要等换上含这处修复的包再测。

**顺手修的**：Windows 上 `detect::which` 只会给名字加 PATHEXT 后缀，`chat.toml` 里写 `command = "powershell.exe"` /
`"node.exe"` 这类带后缀的名字会去找 `powershell.exe.exe`，报"not found"（上面的测试就是这么撞到的）。现在名字已带 PATHEXT 里的
后缀时先按原名找；新增 Windows 单测 `programs_are_found_on_a_path_with_or_without_their_extension`。

## 被控端多屏：会话页切换显示器（2026-10-22，分支 `feat/multi-monitor`，worktree `../ai-remote-monitors`）

起因：用户说"被控端的多屏幕记得适配一下"。win-dev 的 `system_info`（2026-10-22 读到）：序号 0 是右侧竖屏 1440×2560 @ (5120, -9)，
1 是左侧竖屏 1440×2560 @ (-1440, -9)，2 才是主屏 5120×2160 @ (0, 0)；主屏缩放 1.75，两块竖屏 1.25（混合 DPI）。

**原来的样子（读代码）**：会话页从不传 `monitor`，永远推主屏，没有切屏入口。`screen/1` 本来就收 `monitor`（`system_info.monitors`
的序号），回复的 `monitors` 只有 index / width / height / primary，没有原点，也不说推的是哪块。被控端其余部分对非主屏本来就对：
- Windows 上 `monitors()`（windows_capture）、`geometry()`（xcap）和截屏用的 `win::monitor`（windows_capture `from_index`）都是同一个
  `EnumDisplayMonitors` 顺序，宽高都是 `dmPelsWidth`、原点是 `dmPosition`；被控端进程是 PerMonitorV2，所以都是物理像素。
  DDA 按 `HMONITOR` 找对应的 DXGI 输出。macOS 都用 xcap 同一列表、`CGDisplayBounds` 点坐标。
- 光标：`GetCursorInfo` 是物理像素，`PointerFeed` 减去推流那块屏的原点；前端 `PointerLayer.move` 在 `0..width` / `0..height` 之外就隐藏。
- 输入：前端 `RemoteInput.toScreen` 用回复的 `screen` 矩形换算（负原点照样对），滚轮先 move 再 wheel；被控端注入走
  `MOUSEEVENTF_VIRTUALDESK` 再用 `SetCursorPos` 校正，`virtualdesk_*` 单测早就用 win-dev 的 (-1440, -9, 8000, 2560) 布局。

**改了什么**：
- 被控端 `stream/capture.rs`：`monitors()` 两个平台都用 xcap，返回 `MonitorInfo`（index、x、y、width、height、primary），和
  `system_info` 是同一个列表。`stream/protocol.rs`：回复的 `monitors` 每项带 `x` / `y`；新增 `monitor`（实际推的那块：请求的那块，
  没指定就是主屏，没有标主屏时取第一块）；请求了不存在的序号，两个平台统一回 `there is no monitor <n> on this computer: it has N (0 to N-1)`；
  审计 `view_start` / `view_end` 的 `monitor` 记实际那块（以前没指定时是 null）。光标相对坐标抽成 `relative()`。单测 3 个。
- 桌面端 `lib/monitors.ts`：按和主屏的相对位置起名（主屏 / 左侧竖屏 / 右侧屏幕 / 上方 / 下方；同一侧多块按从左到右编号；
  旧被控端没有原点时叫"屏幕 N"）；下拉按桌面上从左到右排；按电脑记在 `localStorage["ai-remote.stream-monitor"]`；
  `isMissingMonitor` 认三种说法（新被控端、旧 Windows 的 `no such monitor`、旧 macOS 的 `monitor not found`）。
- `Session.tsx`：`Stream` 加 `monitor` / `onMonitors` / `onMonitorMissing`，`monitor` 进 effect 依赖，切屏就是重开一条流（和换画质
  一样：清画面、清 `screen`，接管时的输入层先卸掉，新流的第一帧和几何到了再装上，不会把输入按旧屏的矩形发出去）。
  标题条和全屏顶栏在画质下拉左边加"显示器"下拉，用同一套 `nt-select-wrap` / `nt-select`，被控端只有一块屏时不出现。
  记住的那块屏不在了：回到主屏、忘掉这台电脑的选择、提示 `monitor_gone`。演示后端给了三块屏（主屏 1600×1000、左侧竖屏、右侧屏），
  浏览器预览里能看到下拉。
- 被控端 `stream/capture/dda.rs` + `win.rs`（真机上发现，见下）：DDA 交出的画面是面板扫描方向，竖屏横躺。`duplicate()` 带回
  `DXGI_OUTDUPL_DESC.Rotation`，`upright()` 选视频处理器的顺时针转角（`ROTATE90/180/270` 对应 `ROTATION_90/180/270`，和微软
  DDA 示例的映射一致），`Converter::rotated` 里 `VideoProcessorSetStreamRotation`（先查 `FEATURE_CAPS_ROTATION`），推流尺寸按
  转正后的宽高算。单测 `rotated_outputs_are_turned_upright`；`#[ignore]` 的 `every_monitor_comes_out_upright` 在真机上把每块屏
  经 DDA 和 WGC 各截一帧、核对方向、把亮度平面写成 PGM 供肉眼看。
- 桌面端：旧被控端不给原点时，竖屏叫"竖屏 N"（`monitor_n_portrait`），横屏仍是"屏幕 N"。
- 文档：`docs/architecture*.md` 的 `screen/1`、`docs/desktop-design.md` 的会话页顶栏。

**实测（本机 macOS，2026-10-22，合并 main 54169f2 之后，各 1 次）**：
- `cargo fmt --check` 过；`cargo clippy --workspace --all-targets --locked -- -D warnings` 无警告；`cargo test --workspace --locked`
  402 过 0 失败 8 忽略。`dda.rs` 只在 Windows 编译，另在 win-dev 上跑：`cargo fmt --check` 过、`cargo clippy -p ar-host --all-targets --locked
  -- -D warnings` 无警告、`cargo test -p ar-host --lib stream::` 25 过 10 忽略（含新单测）。
- 前端：本文件开头列出的 10 个 `node --test` 文件 118 项 116 过 2 跳过（`stream-ui.test.mjs` 新增 6 项：命名与排序、记住选择与识别
  "屏不在了"、请求带 `monitor` 与回复解析、切屏重开流与回退主屏、标题条和全屏顶栏都有下拉、光标层在负原点竖屏上的位置与移出隐藏；
  `manual-input.test.mjs` 新增 1 项：负原点竖屏上的移动、滚轮和点击落点）；`tsc --noEmit` 过。
- 浏览器预览（`vite --port 1437` + Camofox，演示后端）：标题条出现"主屏 · 1600×1000"，选项依次是"左侧竖屏 · 1080×1920 / 主屏 ·
  1600×1000 / 右侧屏幕 · 1920×1080"；切到左侧竖屏后画面变成竖的、居中；全屏顶栏里同样有这个下拉，和画质下拉同款；刷新后仍是左侧
  竖屏；英文文案也核对过。这是 Firefox 内核里的原生 select，不是 Tauri 的 WKWebView / WebView2。

**真机实测（2026-10-22 18:25–18:45，Mac debug 桌面包 → 真实控制端 → win-dev 服务，服务程序是 17:42 换上的那份，没有本分支的改动）**：
做法：debug 包带 `AI_REMOTE_SMOKE_SCRIPT` 往页面注入驱动脚本（`AI_REMOTE_SMOKE_VISIBLE=1`，窗口出现在 Mac 屏上但不抢焦点），
`AI_REMOTE_CLI` 指向安装版、不设 `AI_REMOTE_HOME`，复用正在跑的控制端，不起新实例。win-dev 两块竖屏上各盖一个置顶的 WinForms
记录窗口（进程设 PerMonitorV2，记录物理像素的指针位置、点击、滚轮、按键），测试的点击和按键只落在它上面。
- 切屏：真实 app 里下拉 3 项"屏幕 1 · 1440×2560 / 屏幕 2 · 1440×2560 / 主屏 · 5120×2160"（旧服务不给原点，所以没有左右），默认主屏；
  切左、切右都重开了流（审计 `view_start` / `view_end` 的 monitor 依次对上）；进全屏后顶栏有下拉，在全屏里切回左屏生效，退出全屏正常。
  两轮都走通。
- **发现 1：服务模式下竖屏横躺**。服务是 SYSTEM，截屏走 DDA；两块竖屏推来都是 2560×1440，桌面转了四分之一圈（左屏测试窗口的字
  竖着、从右上角往下排）。被控端日志 `DXGI_MODE_ROTATION(4)`（`ROTATE270`）。已在本分支修（见上）。修后在 win-dev 以登录用户跑
  `every_monitor_comes_out_upright`：两块竖屏 DDA 720×1280、WGC 720×1280，导出的画面方向一致、测试窗口的字在左上角正着；主屏两条路
  都是 1280×540（1 次）。经服务推流的端到端见下面"端到端"。
- **发现 2：服务模式下同一块屏只能有一个观看者**。第二个观看者的 `DuplicateOutput` 返回 `E_INVALIDARG (0x80070057)`（文档列的原因之一：
  本进程已在复制这个输出），回退 WGC 又是"not supported here"（SYSTEM 不能用 WGC），最后 CPU 路径报
  `Failed to convert item to GraphicsCaptureItem`——背景里提过的那个错就是它。审计证实两次失败时都有另一个观看者（同一台 Mac 的控制端）
  正在看主屏。没修，列为后续。
- 输入：接管左竖屏后，第一次移动（画面中心）在 win-dev 上读到的指针位置是 (-720, 1271)，和预期完全一致（1 个点）。其余移动、点击、
  滚轮、按键都被被控端以"an ai-remote approval prompt is open on the host, so input is paused"拒绝（审计 `input_end` 三次分别是
  moves=1 errors=3、moves=1 errors=5、moves=0 errors=5），记录窗口没收到。
- **发现 3：审批超时后对话框不关，实时输入一直暂停**。屏上两个"ai-remote: approve an action"是另一个会话的 `chat_start`
  （17:56:14、18:38:31 发起），审计里都已 `approval_resolved verdict=timeout`（60 s），但两个窗口到 18:45 还开着；被控端只看"有没有
  审批框开着"，于是 18:39 之后没有任何待批请求，接管的人照样不能操作。没修，列为后续。用户让点"否"关掉，但这两个窗口属于 SYSTEM
  进程，从 zremote（管理员、用户会话）用 UI 自动化只能看到窗口、看不到按钮（Windows 不让低完整性级别的进程操作 SYSTEM 的窗口）；
  下面换服务程序时旧进程退出，窗口随之关闭，请求早已按超时处理，没有批准任何东西。

**端到端（2026-10-22 19:03–19:05，用户同意后把 win-dev 服务临时换成本分支 006bc71 的 release 构建，测完换回）**：
- 换法：停服务后 `--chat-runner` 子进程（以用户身份跑编程智能体）不退出，一直占着 `ai-remote-svc-bin\ai-remote-hostw.exe`，原地覆盖失败
  （它是有意跨重启保留的：新被控端起来后日志 `the agent runner connected pid=66568`）。于是把新程序放到 `ai-remote-svc-bin-pr35`，
  临时改注册表 `HKLM\SYSTEM\CurrentControlSet\Services\ai-remote-host-cc166525\ImagePath` 指过去；测完停服务、改回原值、再启动。
  服务前后共停了两次，每次约 1 分钟；原程序哈希前后一致；临时目录、备份和 win-dev 上的临时 worktree 都已删除。
- 新回复：CLI `ai-remote view --json` 里每块屏带 `x` / `y`；审计里默认主屏的观看记成 `monitor=2`（实际推的那块）。
- 真实 app：下拉按位置命名、从左到右排："左侧竖屏 · 1440×2560 / 主屏 · 5120×2160 / 右侧竖屏 · 1440×2560"。依次切左、右、主、左：
  竖屏画面 1440×2560、在舞台里竖着居中，测试窗口的字正着在左上角（截图看过）；主屏 5120×2160。
- 接管后的输入（记录窗口记下的物理像素，1 轮）：左竖屏四次移动落在 (-1440, -9)、(-1, 2550)、(-1080, 1910)、(-720, 1015)，和预期完全一致；
  点击按下 / 抬起都在 (-720, 1015)；滚轮 -240（2 格）落在 (-1010, 1526)，预期 (-1008, 1526)；按键 x、y、z 都收到（记成大写是因为
  win-dev 开着大写锁定）。右竖屏移动 (5120, -9)、(6559, 2550)、(5840, 1015) 一致，点击 (5840, 1015) 并激活了右侧窗口，滚轮在
  (5550, 1526)（预期 5552），按键都收到。滚轮差 2 像素是合成 `WheelEvent` 的坐标只有整数 CSS 像素（此时 1 CSS 像素约 3.4 屏幕像素），
  不是换算问题；真鼠标没测。全程没有报错，最后交还控制成功。
- 记住的屏不在了：把 win-dev 记成第 7 块屏再打开会话，被控端回 `there is no monitor 7 on this computer: it has 3 (0 to 2)`，页面回到主屏、
  提示"上次选的显示器已不在，改为显示主屏。"、清掉了记住的选择。
- 远端光标叠加层：win-dev 的光标处于抑制状态（`GetCursorInfo` flags=2），被控端一直报不可见，叠加层全程隐藏；从 zremote 注入的
  相对移动也清不掉这个状态。所以叠加层在侧屏上的位置真机没测，靠单测。

**坑**：
- zsh 里 `"VAR=值" 命令` 不算环境变量赋值（`command not found`），带 `&` 的值用 `env 'VAR=a&b' 命令`。
- `window.__TAURI_INTERNALS__.invoke` 不能替换（赋值不生效），截不到推流回复；预期坐标直接用 `system_info` 的矩形。
- 合成的 `pointerdown` 没有真实指针，`setPointerCapture` 会抛错、`RemoteInput` 的处理到不了发送那一步：脚本里先把它包一层 try。
- debug 包和安装版是同一个 bundle id，`localStorage` 共用：脚本结束时删掉 `ai-remote.stream-monitor` 里的 win-dev 和
  `ai-remote.video-trace`（smoke 模式会写这个键）。可见窗口用户能点到（第三轮被点了一下，跳回主屏、切到了对话栏）。
- win-dev 是多个会话共用的：别的会话的观看和审批框会直接影响测试，先看审计（`audit.jsonl` 的 `view_*`、`approval_*`）。

**推导（没有真机数据）**：远端光标到主屏（x ≥ 0）时，相对左竖屏的 x ≥ 1440，叠加层隐藏（单测覆盖，真机光标一直不可见）。

**未测**：真鼠标拖动（只用了合成事件，没有拖动）；光标叠加层在侧屏上的位置（win-dev 光标被抑制，被控端一直报不可见）；混合 DPI 下
侧屏光标形状的大小；锁屏后切屏；`ROTATE90` / `ROTATE180` 的屏（win-dev 两块都是 `ROTATE270`，另两种只有单测和微软示例的映射）；
macOS 被控端多屏（这台 Mac 没接外屏）；Windows 控制端（WebView2）里下拉的样子。win-dev 的服务现在仍是原来的程序，不含本分支。

**和 PR #34（`fix/fullscreen-cursor`）**：两边都改了 `Session.tsx`。#34 先合进了 main；本分支合并 main 9933900 时只有 `Stream` 的
`callbacks` 那两行冲突，取并集：`{ onMonitors, onMonitorMissing, onFrame, onSize, onScreen, onStats, onRestart, onError, onPointerShown }`，
合并后 `tsc --noEmit` 过，前端 10 个测试文件 119 项 117 过 2 跳过。远端光标移出当前屏时叠加层隐藏（本分支）、本地鼠标随之重新出现（#34 的
`onPointerShown`）。

**和 PR #38（`fix/dda-shared-viewers`，同一块屏多个观看者共用一个 DDA 复制）**：#38 先合进了 main；本分支合并 main 37a4fcc 时 `dda.rs` 冲突，
照 #38 那一节写的办法解：导入取两边的并集；`run` 用 #38 的结构（`while !source.stopping()`），解构成 `(output, dup, rotation)`，
把 `*rotation` 传给 `Viewer::deliver`；`deliver` 里用 `upright(rotation, input)` 算转角和转正后的尺寸，`SizedConverter` 带上转角比较，
建转换器用 `Converter::rotated`。也就是说每个观看者各自转正（各自的尺寸、各自的转换器）。两边各加的 `#[cfg(test)] mod tests` 合成一个；
git 把我的 `mod tests` 插到了 #38 给 `bench` 写的文档注释和 `mod bench` 中间，已挪开，注释回到 `bench` 上。`bench` 里的两个手动测试
（`every_monitor_comes_out_upright`、`two_viewers_of_one_monitor_both_get_frames`）都保留。合并结果在 win-dev 上单独的目录里
（`%USERPROFILE%\ai-remote-mm-check`，git bundle 送过去，没碰服务）：`cargo fmt --check` 过，`cargo clippy -p ar-host --all-targets
-- -D warnings` 无警告，`cargo test -p ar-host --lib stream::` 31 过 0 失败 11 忽略（含 `rotated_outputs_are_turned_upright` 和 #38 的共享复制测试）。本机 Rust 全量 410 过 0 失败 8 忽略。合并后的竖屏转正
和多观看者没在真机上一起跑过（推导：两条改动只在 `deliver` 里相遇，各管各的）。

## iPhone 对话补齐到桌面端：预览、选目录、按项目分组、输入增强（2026-10-22，分支 `feat/ios-chat-v3`）

桌面端经 #24 合进 main 的对话功能，iPhone 端这次补齐。被控端和协议都没改，只用已有的 `chat_file`、`files_list`、chat/1 的
`files` / `ack` / `commands` / `attachments`。

- **Rust 核心**（`ar-mobile`）：新增 `preview_file(host, session, path)`：调 `chat_file` 拿票据，经内嵌控制端的 `files` 流读进内存，
  核对 SHA-256，不写盘；上限 16 MiB、整体 60 s 超时。其余（`files_list`、`chat_file` 调用）走已有的 `request`。
- **纯逻辑**（`PhoneChatInput.swift`，照 `lib/chat-input.ts`、`lib/chat.ts` 的目录 / 发送状态部分、`lib/preview.ts` 移植，改桌面端时同步改这里）：
  `/` `@` 菜单（光标位置按 UTF-16，和 `UITextView` 一致）、图片限制、行分类（`phoneChatLineKind`）、发送中 / 未送达
  （`PhoneOutgoing` 一组函数）、主控端路径规则（Windows / Unix / `~`）、按项目分组、`phoneTurnFiles` / `phoneFileTarget` /
  `phonePreviewDocument`。`PhoneChatLog` 多了 `inputs`、`dropped`、`commands`、`images`，用户消息带 `input_id` 和附件。
  `PhoneChatMedia.swift`：ImageIO 处理图片（相册 / 相机的 HEIC 等一律缩到长边 2048 转 JPEG，已合规的 PNG/JPEG/GIF/WebP 原样发）。
- **Store**：按行分流（事件 / 命令列表 / 带 `rid` 的回复 / ack 与拒绝 / 其他错误）；发送立刻显示为"发送中…"，收到 `user` 事件或进队列后消失，
  被拒、断线、15 s 无 ack 显示"未送达"，可"重新发送"（断线用原 id，被拒换新 id）或"丢弃"（文字和图片回到输入框）；
  `@` 文件经 `{"files":{rid,..}}`，10 s 超时；浏览目录用 `files_list`（列 `~` 时记下被控端的家目录，路径缩写成 `~`）；
  最近文件夹按主机存在 `UserDefaults`；预览按路径缓存，失败不缓存。
- **界面**：会话列表按项目分组（文件夹名 + 上级路径 + 数量，会话行带"多久前"）；新对话的文件夹是一行，点进 `PhoneFolderPicker`
  （最近 → 浏览：面包屑、`..`、子文件夹、底部"使用「名字」"；输入路径回车直接打开；列不出时给原因和"仍然使用这个路径"）；
  已完成的轮次下面显示它写出 / 点名的文件卡片（图片是缩略图），点卡片、diff 卡片上的眼睛、回复里指向文件的链接都打开预览 sheet；
  输入框一行：左 `+`（从相册选择 / 拍照，智能体不收图片时置灰）、中间文字、右停止 / 发送，上方是待发图片和 `/` `@` 菜单。
- **HTML 预览的取舍**：用 `WKWebView` 显示，但关掉内容 JavaScript（`allowsContentJavaScript = false`），页面最前面插桌面端同一条
  CSP（不联网、只收内联样式和 data 图片），`nonPersistent` 数据存储，第一次加载之后的所有导航都拒绝；另有"源码"视图。
  理由：只看源码对网页报告没用，静态 HTML/CSS 才是要看的；不跑脚本和桌面端规则一致。iPhone 上的网页视图本来就碰不到应用代码
  （没有注册任何 script message handler），所以这是谨慎而不是堵漏洞，代价是靠脚本画的页面（图表库）只剩静态部分。SVG 也走这个网页视图。
- Info.plist 的相机说明改成同时覆盖扫码和拍照发给智能体；相册用 `PhotosPicker`（系统进程内选图），不需要相册权限说明。

**实测（2026-10-22，本机 macOS）**：
- `tools/test-phone-chat-model.sh` 31 项全过（新增 14 项，用例照搬 `chat-input` / `chat-folder` / `chat-preview` / `chat-state` 的 JS 测试）。
- `$HOME/.local/bin/python3.12 tools/phone-chat-smoke.py`（隔离被控端 + 替身智能体，三个 home 都 `[network] quiet = true`）53 项全过（1 次）。
  新增：记住最近文件夹、hello `acks`、命令和技能列表、`@` 文件（多词匹配）、发图片后替身收到的 base64 长度与原图一致、发送中列表在对话出现后清空、
  5 张图被被控端拒绝后显示原话并可丢弃、浏览会话文件夹（隐藏目录不列）、列 `~` 记下家目录、不存在的文件夹报原话、回复点名的三个文件、
  预览会话目录里的 HTML 逐字节一致、PNG 可解码、目录外但智能体点名的文件可预览、没点名的被拒、`.toml` 被拒。
- iOS 模拟器（自建 `ios-chat-v3`，iPhone Duo / iOS 27.1，xcodegen 生成工程）`PhoneChatUITests` 3 个用例全过（演示数据，没连被控端）。
  新用例走：轮次下的三张预览卡片、图片大图、网页（静态内容出现、"脚本没有运行"仍在 = 脚本没跑、外部图片没加载）、源码视图、Markdown 文档、
  回复里的"报告"链接打开预览、`/re` 弹命令菜单并选中、`@rep` 弹文件菜单并插入路径、第一次发送被拒（`-ui-chat-unacked`）显示未送达后重发成功且气泡带缩略图、
  会话按项目分组、文件夹选择器从最近进入浏览、逐级进入后"使用"回到表单。截图 `/tmp/ios-chat-v3/01~13`，都看过（没进仓库）。
- `cargo fmt --check`、`cargo clippy -p ar-mobile --all-targets -D warnings`、`cargo test -p ar-mobile` 过。

**未测**：真机（相册选图、拍照、HEIC 转换只在 Mac 上用 ImageIO 测过 PNG）；模拟器 UI 测试是演示数据，真实被控端上的界面只经过冒烟（同一套 Store 代码在 macOS 上）；
Windows 被控端路径（`D:\`）只有单测；`files_list` 被机主改成询问 / 拒绝时的浏览体验；超过一页（500 项）的文件夹只提示不翻页；
粘贴图片（`UITextView` 粘贴）、查找、选中引用、上下文用量明细、子智能体内部步骤这几项桌面端有、这次没做。

## 服务模式下同一块屏多个观看者：共用一个 Desktop Duplication（2026-10-22，分支 `fix/dda-shared-viewers`）

起因：PR #35 在 win-dev 真机上的"发现 2"（见 `feat/multi-monitor` 那一节）。服务模式（SYSTEM，截屏走 DDA）下第二个观看者看
同一块屏失败，看到的是误导性的 `Failed to convert item to GraphicsCaptureItem`。

**原因（读代码 + 微软文档）**：每个观看者的 `capture::start` 各自 `dda::start`，各自 `IDXGIOutput1::DuplicateOutput`。
`DuplicateOutput` 的文档把"调用进程已经在复制这个输出"列为 `E_INVALIDARG` 的原因之一，所以同一进程里每块屏只能有一个复制，
`MAX_VIEWERS = 3` 在 DDA 下实际是每块屏 1 个。DDA 失败后回退 WGC（SYSTEM 用不了：`not supported here`），再回退 CPU 路径
（也是 WGC：`Failed to convert item to GraphicsCaptureItem`），观看者只看到最后这条。同一块屏快速重开（换画质、重试）也可能
撞上旧复制还没释放。

**改了什么**：
- `stream/capture/shared.rs`（新，和平台无关，Mac 上也跑测试）：按 key（显示器）登记正在跑的采集源。第一个观看者启动它，
  后来的加入，最后一个离开时停掉它，并且**拿着注册表的锁等采集线程结束**：这样马上重连的观看者会等旧的复制放手，
  而不是撞上 `E_INVALIDARG`。采集线程从不拿注册表的锁，所以不会死锁。
- `stream/capture/dda.rs`：每块屏一个复制线程（`static DUPLICATIONS`，key 是 `HMONITOR`），每个观看者有自己的帧率、长边上限
  （编码器可能会调低）、`Converter`（含纹理池）和 slot；每帧对每个观看者各转换一次。一个观看者的转换器建不起来只关它自己的
  slot，不影响别人。复制失败重试时的 500 ms 等待改成可打断（`pause`），最后一个观看者离开时不用干等。共用一个 D3D 设备：
  多个观看者的硬件编码器在同一个（多线程保护的）设备上。
- `DuplicateOutput` 失败时按错误码说人话（`why_refused`）：`E_INVALIDARG` → 这块屏的 DDA 已被本被控端占用（另一个观看者）；
  `DXGI_ERROR_NOT_CURRENTLY_AVAILABLE` → 同时用 DDA 抓这块屏的程序太多（Windows 最多 4 个）；`E_ACCESSDENIED`、
  `DXGI_ERROR_UNSUPPORTED`、`DXGI_ERROR_SESSION_DISCONNECTED` 各有一句；其他码保持 `duplicate the output: ...`。
- `stream/capture.rs`：以 SYSTEM 运行时 DDA 失败就直接报它的错（`capture the screen with Desktop Duplication: ...`），
  不再落到 WGC / CPU 两条注定失败的路。非 SYSTEM（用户态被控端在安全桌面上、或 `AI_REMOTE_CAPTURE=dda`）仍回退 WGC。
- 文档：`docs/operations*.md` 服务模式一段补了"同一块屏最多 3 个观看者共用一个复制"。

**测试**：
- `capture::shared::tests` 5 项（Mac 和 Windows 都跑）：同屏共用一个采集源、各自收到帧、一个离开后另一个照收；最后一个离开时
  等采集线程结束；启动失败不留残骸；**马上重连的观看者等旧采集源放手**（模拟 DXGI 规则：旧的还占着输出时再启动就失败；
  旧的放手要 50 ms，新观看者第 10 ms 进来）；4 个线程反复进出的压力测试。变异检查：把"拿着锁等线程"改成先放锁，
  重连那一项失败（压力测试抓不到，所以才加了确定性的那一项）。
- `dda::tests::refusals_say_why_in_words`（只在 Windows）：`E_INVALIDARG` 的文案和 `0x80070057` 都在。
- `dda::bench::two_viewers_of_one_monitor_both_get_frames`（`#[ignore]`，手动在 Windows 真机跑，不用服务、以登录用户身份）：
  主屏上两个观看者（长边 1280 / 640）同时出帧且尺寸各自；第一个离开后第二个继续出帧；最后一个离开后马上来的新观看者能出帧。
  改之前第二个 `start` 应当直接 `E_INVALIDARG`（同一进程复制两次），所以不用服务也能复现。
  `cargo test -p ar-host --lib two_viewers_of_one_monitor -- --ignored --nocapture`

**实测（本机 macOS，2026-10-22，各 1 次）**：`cargo fmt --check` 过；`cargo clippy --workspace --all-targets --locked -- -D warnings`
无警告；`cargo test --workspace --locked` 404 过 0 失败 8 忽略（含 `capture::shared` 5 项）；前端 10 个 `node --test` 文件 111 项
109 过 2 跳过；`tsc --noEmit` 过。`dda.rs` 只在 Windows 编译：win-dev（Windows 11，debug，独立 worktree
`%USERPROFILE%\ai-remote-ddashare`，等 #35 的会话测完、交还机器后才跑，没碰服务）上 `cargo fmt --check` 过、
`cargo clippy -p ar-host --all-targets --locked -- -D warnings` 无警告、`cargo test -p ar-host --lib stream::` 27 过 0 失败 10 忽略
（含 `refusals_say_why_in_words` 和 `capture::shared` 5 项），各 1 次。

**真机实测（win-dev，Windows 11，主屏 5120×2160，2026-10-22 20:21–20:35，用户同意后做的，期间租着 win-dev）**：
- 不用服务也能复现根因：main（54169f2）加上同一个手动测试，以登录用户身份跑，第二个 `start` 报
  `duplicate the output: 参数错误。 (0x80070057)`（1 次）。本分支跑 3 次都过：日志 `another viewer joined this monitor's
  Desktop Duplication viewers=2`，两个观看者 1280×540 / 640×270 同时出帧，第一个离开后第二个继续出帧，最后一个离开后马上来的
  新观看者 130 / 633 / 58 ms 出首帧。
- 经服务端到端（Mac 上安装版的控制端 `ai-remote view`，HEVC，30 fps）。换之前（原服务程序）：第一路 1280×540 28 fps，4 s 后
  开的第二路报 `start screen capture: Failed to convert item to GraphicsCaptureItem`，复现了原问题。临时把服务换成本分支的
  release 构建（新程序放在 `%USERPROFILE%\ar-pr38\bin`，只改注册表 ImagePath，原 ImagePath 存在 `ar-pr38\imagepath-original.txt`；
  win-dev 防火墙三个配置文件都关着，不用加规则）之后，各 1 次：
  - 两路同看主屏：都出帧，1280×540 29.8 fps（首帧 122 ms）、640×270 29.8 fps（首帧 80 ms）。
  - 三路：当时还有一个不是测试发起的观看者（同一台 Mac 的控制端，服务重启后自己连回来的）在看主屏，所以我的两路加它正好
    3 个，host.log 出现 `viewers=3`，三路都出帧（1920×810 29.9 fps、1280×540 29.9 fps）；我的第三路是第 4 个，被
    `MAX_VIEWERS = 3` 以 "too many people are watching this computer right now" 拒绝，符合设计。
  - 第一路看着的同时第二路连开两次：第二次同样因为满 3 个被拒（前一次的观看者槽位还没释放加上那个外部观看者）；第一路结束后
    马上开的新一路 87 ms 出首帧，没有撞上旧复制。
  - 测试期间 host.log 没有 WARN / ERROR。
- 测完换回：ImagePath 恢复、两个程序的 SHA-256 与测试前一致（hostw `82A9C78A…`、ai-remote `52C8AB20…`），chat-runner 重新连上，
  Mac 上 `ai-remote view` 3 s 出 88 帧。chat-runner 停服务时不退出、占着 `ai-remote-svc-bin` 里的程序，所以换服务程序要放到别的
  目录、改 ImagePath，不能原地覆盖（#35 的会话发现的）。
- win-dev 上留着 `%USERPROFILE%\ai-remote-ddashare`（本分支）和 `ai-remote-ddamain`（main + 手动测试补丁）两个 worktree 及
  `ar-pr38` 目录，以后可删。

**未测**：多个观看者共用一个设备时的编码耗时（推导：每帧每个观看者多一次视频处理器转换，量级 1 ms 以内，没测）；两个观看者看
不同的屏（命令行 `view` 没有选屏参数）；锁屏 / 安全桌面上的多观看者；显示配置变化后 `HMONITOR` 可能换值，key 用的是
`HMONITOR`，这种情况下新旧复制线程的行为和改之前一样（没处理、没测）。

**和 PR #35（`feat/multi-monitor`）**：两边都改了 `dda.rs` 的 `start` / `run`。#35 给 `duplicate()` 加了第三个返回值
`rotation`、`SizedConverter` 加了转角、建转换器改成 `Converter::rotated(..., turn)`。合并时：`duplicate()` 用 #35 的签名、
保留本分支的 `.map_err(refused)`；`run` 用本分支的结构，`let Some((output, dup, rotation))`，把 `*rotation` 传给
`Viewer::deliver`；`deliver` 里照 #35 用 `upright(rotation, input)` 算 `(turn, (width, height))`，按转正后的宽高算
`stream_size`，`SizedConverter` 带上 `turn` 比较，`Converter::rotated(gpu.clone(), input, output, self.fps, turn)`。
两边各加了一个 `#[cfg(test)] mod tests`（合并后会重名，合成一个），`bench` 里两个手动测试都留着。2026-10-22 在临时
worktree 里按这个办法试合并过：没有冲突标记、`cargo fmt --check` 过；`dda.rs` 在 Mac 上编不到，合并结果没编译过。

## iPhone 版的网络设置：安静 / 仅限局域网（2026-10-22，分支 `feat/ios-network`，worktree `../ai-remote-ios-net`）

起因：2026-10-22 在 Mac 上抓包，模拟器里的 iPhone 版一直在连 n0 公共中继（UDP 7842 探测约 2.7 包/秒、4 台中继 TCP 443、
`dns.iroh.link`），同时段桌面端和被控端只在两台机器之间走 UDP 47800。

读代码：iPhone 版的 Rust 核心（`ar-mobile` → `ar_controller::EmbeddedController::start`）本来就和桌面控制端一样读
`<应用数据>/controller/ai-remote.toml` 的 `[network]`（`NetworkOptions::from_config`），所以**默认已经是安静的**（不做 mDNS、
不做 UPnP、公共 DNS 只在系统没配 DNS 时用；模拟器里旧数据的 mesh 配置也确认是 `local_discovery = "off"`、`port_mapping = false`）。
缺的是：手机上没有任何入口写这个文件（模式只能是自动，所以一直连中继），`set_host_address` 被 `MobileController::request`
的白名单挡着，界面也看不到电脑的地址。

- `ar-mobile` 导出两个同步函数：`network_settings(directory)`（`mode` / `lan_discovery` / `port_mapping` / `locked` / `pair_code` /
  `lan_port`，用 `NetworkOptions::from_config` 算实际生效值，`quiet` 时两个开关报关）和 `update_network(directory, mode?,
  lan_discovery?, port_mapping?)`（打开前 `ensure_not_quiet`；手机的局域网发现写 `"resolve"`：只找电脑、不广播自己）。
  `request` 白名单加 `set_host_address`。配置在核心启动时读，所以 Swift 侧 `PhoneClient.saveNetwork` 写完就 close + start。
- iOS：电脑列表左上角 **网络**（`PhoneNetworkSheet.swift`）：连接方式两行单选（自动〔推荐〕/ 仅限局域网）、可选功能两个开关
  （仅限局域网时隐藏 UPnP）、`quiet` 锁定说明；仅限局域网时列出每台电脑的地址，可修改（`PhoneAddress.validInput` 先校验，
  规则和桌面端 `lib/network.ts` 一样，并且更严：`10.0.0.5:` 这种空端口也拒绝）。文案照桌面端 `nw_*`，"这台电脑"改成"这台 iPhone"。
  仅限局域网时电脑列表标题下显示"仅限局域网"标签；配对页在仅限局域网又没开发现时说明配对码找不到电脑，给"改用完整票据"和
  "打开网络设置"；Rust 的英文报错（`PAIR_CODE_NEEDS_DISCOVERY`、仅限局域网下的 `did not answer`）在 `PhoneNetwork.explain` 里换成中文提示。
- 测试：`crates/ar-mobile/tests/network.rs`（默认安静且不写文件、仅限局域网无配对码、`resolve`、quiet 锁与保留注释、按配置启动的
  控制端 `lan_direct = true`、`set_host_address` 过了白名单）；`apps/ios/Tests/PhoneNetworkTests.swift`（模型与地址规则，用例同
  `tools/network-ui.test.mjs`）；`PhoneNetworkUITests.testLocalNetworkOnlyIsOneTapAndPairCodesExplainThemselves`（真实核心：
  默认两个开关关、一键切仅限局域网、UPnP 隐藏、标签出现、配对页说明，最后切回自动）。
- `PhoneNetworkUITests.testLocalNetworkOnlySessionForCapture` 是测量用的，默认跳过：在已配对真实电脑的模拟器上通过界面切到
  仅限局域网、改电脑地址、连进去保持 N 秒，配合抓包。

实测（2026-10-22，本机 macOS，公司网络，iOS 26.2 模拟器，debug app + release 核心）：
- `xcodebuild test` 全部 24 个用例：23 过、1 跳过（测量用例）；`cargo test -p ar-mobile` 5 个过。
- 抓包：`tcpdump -i pktap,all -k NP`（按进程标注；`udp` + TCP SYN，另抓一路 `port 53` 看 DNS 名字）360 s，root 由
  `osascript ... with administrator privileges` 弹窗授权。被控端是一个隔离的临时被控端（`/tmp` 下独立 `--home`，`mode = "lan"`、
  `port = 47810`、`quiet = true`），没动这台 Mac 上正在用的被控端。app 先在容器里预设 `mode = "lan"`（和界面写的一样），然后由
  测量用例全程走界面：粘贴票据配对（27 s）→ 网络页改地址为本机局域网地址的 47810 端口 → 打开电脑，40 s 出画面，保持 180 s。
  结果：app 进程只和被控端（本机局域网地址:47810）互发 UDP（app 发 36,025 个、收 95,235 个，14:25:12–14:28:13；启动到配对前、
  会话结束后都是 0 个）；**任何进程发往 4 台 n0 中继和 `dns.iroh.link` 解析出的地址的包：0**；**DNS 里含 `iroh` 的查询：0**；
  app 发往 5353 / 1900 / 5351 / 7842 / 53 / 443 / 80 的包：0；app 的 TCP SYN：0。
  对照：父会话同一天用同样的命令抓到自动模式下的 app 3 分钟 479 个 UDP 7842 包，说明方法能抓到这类流量。原始抓包已删。
- 坑：新建的 iOS 26.2 模拟器没有中文字体，截图里中文是方块（布局照样能看）；`xcodebuild test` 跑完测量用例后会卡在收尾
  （结果包没写完），测试日志里结论已全，kill 掉即可；测量第 201 s 起设备方向被外部改了几次（不是测试自己转的，原因未查），之后会话页的返回按钮找不到，
  所以测量用例不再点返回，靠 `terminate()` 结束。

限制：局域网发现用的是原始 mDNS 组播，按 Apple 文档真机需要 `com.apple.developer.networking.multicast` 权限，本项目没有申请，
真机上大概率不工作（未验证），仅限局域网下真机请用完整票据配对。真机上的仅限局域网连接未测（模拟器和 Mac 共用网络栈，
不经过 iOS 的"本地网络"权限）。新模拟器跑 UI 测试时 app 默认是自动模式，会连几分钟 n0 中继（和 CI 一样）。

复现：`tools/ios.sh framework`、`cd apps/ios && xcodegen generate`，然后
`TEST_RUNNER_AR_CAPTURE_ADDRESS=<ip>:<port> TEST_RUNNER_AR_CAPTURE_SECONDS=180 TEST_RUNNER_AR_CAPTURE_TICKET="$(cat ticket)" xcodebuild ... -only-testing:AIRemoteUITests/PhoneNetworkUITests/testLocalNetworkOnlySessionForCapture test`，
同时另开 root 的 pktap 抓包。

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
- **SYSTEM 身份的被控端**（Windows 服务模式）：自己不执行任何东西，交给用户会话里的 chat-runner（见下一小节）。**root 身份**（macOS 登录窗口）仍是 `can_start: false`。原因相同：智能体必须以用户身份运行；用 SYSTEM / root 身份去执行用户目录里的程序等于本地提权。
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

8. `chat.toml` 写坏时，原先退回默认值，也就是"允许"：文件里原本写着 `enabled = false` 也会被放开。现在改成：写坏时智能体一律关闭；原因显示在 `chat_runtimes` 的 `host.config_error`、doctor、桌面端和命令行；`chat enable|disable` 不会覆盖写坏的文件。我自己测试时就踩到了：拿内置 id `opencode` 作自定义 id，整个文件因此被拒。
9. 真实 Tauri 应用只编出了内嵌前端的调试版（`npx tauri build --debug --no-bundle`），也用隔离配置启动了（`AI_REMOTE_HOME` 加 `AI_REMOTE_CLI`），但没能点击实测：正式版、本分支调试版和其他会话的调试版 bundle id 相同，电脑操控会认错窗口。对话通道这一层已经由命令行和冒烟测试覆盖；webview 里 Channel 的这一段还没有实测。

### 运行中插话（steer）与事件补齐（2026-10-21，分支 `claude/host-steer-27deef`，叠在 `feat/host-chat` 上）

用户要对话界面接近 Claude / Codex 原生体验，其中一项是回答过程中随时插话。本节只改被控端、命令行和冒烟脚本；界面由另一个会话按下面的协议接。

**协议**（和界面会话约定好的名字，不改）：

- `caps.steer`：claude、codex 为 true，ACP 智能体为 false；`chat.toml` 自定义条目可写 `steer = false`（只能关）。
- chat/1 `input.mode = "steer"`：轮次运行中且支持时送进当前轮，**智能体收下后**才发 `{"t":"user",...,"steer":true}`，信封 `turn` 是当前轮；不支持、没有在跑的轮次、正在打断时排队（空闲直接开新一轮），并发 `notice` `steer_queued` 说明原因。
- `usage` 新增可选 `context_window`、`context_used`。`chat_turn` 审计多了 `steers`（条数）。
- 命令行 `ai-remote chat send <会话> <文本> --steer`；打印时插话显示为 `> (steer) …`。

**实现**：驱动层（`chat/mod.rs`）新增 `Cmd::Steer`、`LiveCmd::Steer`，适配器用三种回答告知结果：`Out::Steered`（并进当前轮）、`Out::SteerLate`（当前轮已结束、智能体自己会把它当下一轮跑，被控端**不再重发**，放在队首并标记 delivered）、`Out::SteerRejected`（退回队列）。进程退出时，未回答的插话和 delivered 的消息都退回队列，由下一个进程重发。

- **Codex**：`turn/steer`，参数 `threadId`、`expectedTurnId`（必须是当前轮）、`input`、`clientUserMessageId`，返回 `{turnId}`；轮次结束后再发返回 `-32600 no active turn to steer` → 退回队列。`turn/start` 还没回 id 时先缓存，拿到 id 再发。方法名和参数来自本机 `codex app-server generate-json-schema`（0.163）。
- **Claude Code**：加 `--replay-user-messages`，每条用户消息被收下时回显一行 `{"type":"user","isReplay":true,...}`。回显在本轮 `result` 之前 → 并进当前轮；到 `result` 时还没回显 → `SteerLate`，之后 Claude 会先出新的 `system init`，再把它当一轮跑（自带 `result`）。
- **ACP**：协议里没有往进行中的 `session/prompt` 追加内容的办法，`steer: false`。
- **Codex 事件补齐**：`item/commandExecution/outputDelta` 流式输出（`tool_update` 不带 `status`、一段文本；驱动层把连续片段合并，每条命令最多 8 KiB，结束时不再重复整段输出、只补 `(exit code N)`）；文件改动的 unified diff 按 hunk 拆成多个 `diff` 内容（old/new 是该 hunk 的上下文 + 删除 / 上下文 + 新增，界面的增删行数能算对）；`item/fileChange/patchUpdated`、`item/mcpToolCall/progress`；`plan` 条目（计划模式的方案）作为一条消息；`thread/compacted` → `notice` `compacted`；`imageView` / `imageGeneration` / `collabAgentToolCall` 三种工具调用；网页搜索结束时写明搜了什么 / 打开了哪页。待办（`turn/plan/updated`）和 MCP 工具调用原来就有。
- **上下文用量**：Codex 取 `tokenUsage.modelContextWindow` 和 `last.totalTokens`（每次 `thread/tokenUsage/updated`）；Claude 取 `result.modelUsage[模型].contextWindow` 和本轮最后一条 assistant 消息的 input + cache_read + cache_creation + output（每轮一次）。

**Claude 实验**（win-dev，Windows 11，Claude Code 2.1.293，已登录；实验脚本是临时 Python、没入库：起 `claude.cmd`，参数与适配器相同，逐行记录输出时刻；任务"从 1 数到 20，每个数单独跑一次 `sleep 1; echo N`，最后只回答最后打印的数"）：

| 场景 | 样本 | 写入时刻 | 结果 |
|---|---|---|---|
| 第 2 个工具结果后写"改成数到 5" | 3 次（其中 1 次带 replay） | 第 2 个结果之后 | 3 次都只有 1 个 `result`，答 `5`，共 5 次工具调用；带 replay 那次回显在写入后 2.9 s、第 3 个工具结果之后出现（模型正在请求下一步时写入，下一次请求才读到） |
| 数到 3，最终文字开始流出时写"再说一个 BANANA" | 2 次（其中 1 次带 replay） | 最后一次请求进行中 | 2 次都是 2 个 `result`：先 `3`，然后新 `system init`，再 `3 BANANA`；带 replay 那次回显出现在第一个 `result` 之后 |

结论：能插话（`steer: true`），但"插进当前轮"只在模型之后还要再请求时成立；没有这层回显就分不清两种情况，所以必须加 `--replay-user-messages`。

**单测**：`chat::tests` 新增 4 项（插进当前轮、太晚时作为下一轮且不重发、空闲时直接开轮、`steer = false` 时排队），替身 Claude（sh 脚本）会回显、SLOW 轮次里收到新消息就并进去、LATE 轮次 2 s 内不读输入；适配器单测新增 Claude 回显判定、Codex steer（立即 / 等 turn id / 被拒）、输出增量 + 按 hunk 的 diff + 上下文用量。`cargo test -p ar-host --lib chat` 34 项全过。

**e2e 实测**（测试分支 = 本分支 + `test/host-chat-quiet2`（安静模式），隔离被控端与控制端都写 `[network] quiet = true`；`tools/chat-smoke.py --quiet`；debug 构建）：

- Mac（macOS，Codex 0.163，`--model gpt-6.1-sol`）：3 次全过（每次 33 项：替身 27 项 + 真实 Codex 6 项）。真实插话：插话发出后 0.02 s 收到 `steer:true`（3/3），每次只跑 5 条命令、最终答 `5`，整轮 24~29 s；普通任务 4.4~5.7 s，打断 0.0 s 内生效，下一轮记得上文 1.9~2.2 s。
- win-dev（Windows 11，Claude Code 2.1.293，`claude.cmd`）：3 次全过（每次 33 项：替身 27 项 + 真实 Claude 6 项；被控端在终端前台运行，不是计划任务里的 hostw）。真实插话：发出后 3.35 / 4.03 / 4.14 s 收到 `steer:true`（要等正在跑的 `sleep 1` 和下一次模型请求），每次只跑 5 条命令、最终答 `5`，整轮 21~31 s；普通任务 5.8~10.2 s，打断 0.1 s 生效，下一轮记得上文 1.6~4.6 s。

**未测**：ACP 智能体真机插话（设计上不支持，只走单测）；Codex 插话正好撞上轮次结束（`no active turn to steer` 退回队列）只有单测和一次手动调用；Claude 插话后紧接着打断（排队中的那条会被 Claude 当下一轮跑）未测；桌面端界面（另一会话）。

### DeepSeek Harness（2026-10-22，分支 `claude/dsh-agy-7f8142`，叠在 `feat/host-chat` 上）

用户定的优先级：Claude、Codex、DeepSeek Harness、OpenCode、Antigravity。本节补上 DeepSeek Harness。只改了被控端、冒烟脚本和文档，桌面端没动。

**DeepSeek Harness**（id `deepseek`，命令 `dsh`，参数 `--profile acp`，走通用 ACP 适配器）。协议细节读的是它仓库 `packages/acp/acp` 的源码和 README（MIT，只读，没有抄代码）：
- 没有 `session/load`，续会话用 `session/resume`（`initialize` 的 `agentCapabilities.sessionCapabilities.resume`）。ACP 适配器现在按能力选：有 resume 用 resume（不回放），否则 load（回放期间不显示），都没有就新开并发 `context_lost`。
- 没有 modes 和 `session/set_model`：模型是 `configOptions` 里 `category: "model"` 的选项，值是不透明的 `["provider","model"]` JSON。适配器按值、名字或数组最后一项匹配用户填的模型名，发 `session/set_config_option`；匹配不到发 `model` 通知列出可选项；智能体没有 `configOptions` 时照旧 `session/set_model`。
- `usage_update`（`used`、`size`，可选 `cost`）→ `usage` 的 `context_used` / `context_window`。
- 它发的是**整条消息**（带 `messageId`），不流式：`caps.partial` 设为 false。适配器现在按 `messageId`（没有时按"工具调用之后另起一条"）分开消息，一轮的 `final_text` 是最后一条消息。这一点也改变了 OpenCode 等其他 ACP 智能体：以前一轮所有文字拼成一条。
- 登录态：dsh 没有状态命令。看环境变量 `DEEPSEEK_API_KEY`、`$DSH_HOME/.credentials.yaml`（含 `deepseek-account` 视为账号登录）和 `$DSH_HOME/.env`（`$DSH_HOME` 默认 `~/.dsh`），只判断有无、不读出值；连目录都没有报 `logged_out`，其余 `unknown`。`Runtime::login` 因此从"命令参数"改成枚举 `Login`。
- `caps.steer` false（ACP 不能往进行中的 prompt 追加）。
- 工具调用：真机上它每次都发 `kind: "other"`，`title` 只是工具名（`bash`、`read`），命令和路径在 `rawInput`。ACP 适配器在 `kind` 缺省或为 `other` 时按工具名推断类型（bash → execute、read → read、edit / write → edit、grep / glob → search 等），用 `rawInput.command` 或路径作标题，路径作 location。报了具体 kind 的智能体不受影响。模型名匹配也改成不区分大小写（它的选项名是 `DeepSeek-V4-Pro`，值的末项是 `deepseek-v4-pro`）。

**真机（Mac，dsh 0.2.1-rc.2，用户自己配的 key，默认模型 deepseek-v4-flash，2026-10-22）**：
- 直接驱动它的 ACP（`/tmp` 下的临时脚本，没入库）：`initialize` 声明 `sessionCapabilities.resume`；一次"跑 `sleep 3 && echo finished` 再读文件"的任务 4.7 s 完成，消息与思考同一个 `messageId`、`usage_update` 报 `size: 1000000`；`session/cancel` 0.11 s 后 prompt 以 `cancelled` 返回，正在跑的工具以 `failed`（`Error: tool call aborted`）结束；`set_config_option` 切到 `deepseek-v4-pro` 生效；关掉进程后新进程 `session/resume` 立即返回（模型选择保留），下一轮答出上一进程里记住的词。以上各 1 次。
- `tools/chat-smoke.py --quiet --real deepseek=$HOME/.local/bin/dsh`：29 项全过（1 次；第一次跑时因为工具 kind 是 other，"跑 shell 工具 / 打断"两项没过，改适配器后通过）：答题 2.0 s，跑 shell 工具，打断 0.0 s 内生效，下一轮记得上文 0.6 s；`chat list` 的登录态是 `logged_in` / `api_key`。
- Windows（win-dev，Windows 11，debug，本分支以 bundle 检出到独立 worktree `%USERPROFILE%\ai-remote-dsh`）：`cargo clippy --workspace --all-targets -D warnings` 无警告；`cargo test -p ar-host --lib chat` 25 项全过（Windows 上跳过 Unix 的驱动层替身测试）；`tools/chat-smoke.py --quiet` 替身部分 28 项全过（1 次）。
- **dsh 真实对话只在 macOS 测过**（Windows 不配 key）。经被控端 park 之后再续接（`session/resume` 走驱动层）只在单测和上面的协议脚本里覆盖，没有在被控端里端到端测。

**Antigravity：暂不接**（用户没有密钥，2026-10-22 决定先不适配）。已写好的适配器存在本地分支 `claude/agy-adapter-parked`（提交 9012d94，未推送），内容：`agy --input-format stream-json --output-format stream-json --dangerously-skip-permissions` 常驻进程（依据官方文档 antigravity.google/docs/cli/headless，prompt 走 stdin）、`--conversation` 续接、打断即结束进程树、`chat.toml` 的 `base = "acp"`（给 Google 另发的 `agy_acp_server.par` 用），以及单测、替身驱动层测试和冒烟里的替身 agy（都通过过）。以后要接，从那个提交挑 agy 相关部分即可。2026-10-22 两台机器装了 agy 1.4.4（Mac `~/.local/bin/agy`；win-dev `%LOCALAPPDATA%\agy\bin\agy.exe`，官方脚本**不加进 PATH**，接入时要把这两个位置加进 `detect.rs` 的已知位置）。`agy --help` 证实 `--input-format stream-json`、`--conversation`、`--dangerously-skip-permissions` 都在；`--print-timeout` 在 1.4.4 默认是 0（等到这一轮完），搁置分支里传的 `12h` 可以去掉。账号已登录，但 Google 以地区不可用拒绝服务（`agy -p` 输出 `AGY_ERROR: {"status":"PERMISSION_DENIED","error_code":403,...}`，主会话实测），所以**真实成功路径未测：账号地区受限**；接入时要把这行 `AGY_ERROR:` 当作一轮失败、原话显示。

**实测（本机 macOS，2026-10-22）**：
- 单测新增：ACP 2 项（dsh 的 initialize / `session/resume` / 模型配置项 / 整条消息 / `usage_update` / 工具失败 / 结束文字，按它服务端的真实行格式；resume 失败转新会话 + 模型名不存在），detect 1 项（dsh 的 key / 账号 / 空 key / 没有目录）。
- `tools/chat-smoke.py --quiet`（隔离被控端和控制端，真实 QUIC，debug）：28 项全过（新增 1 项：dsh 以 ACP、`steer: false` 列出）。`--real deepseek=<dsh 路径>` 不用改脚本就能用（`--real` 取内置 id）。
- `cargo fmt --check`、`clippy --workspace --all-targets -D warnings` 无警告；`cargo test --workspace`（ar-host 270 项等）只有 `ar-net` 的 `ipc::tests::a_second_daemon_cannot_replace_the_live_socket` 在整跑时失败过 1 次，单独重跑 3 次都过，本分支没改 ar-net，判断是偶发；node 测试 78 过 2 跳过；`tsc` 通过。

**未测**：两台机器都没装 `dsh`，**真实 dsh 一次都没跑过**，协议以源码为准（main 分支，npm 当时 0.2.1-rc.2）。Windows 上没有跑。

### 下一步

- [x] Windows 计划任务（后台 hostw）模式下的真机对话、子进程是否弹控制台窗口：真实 Codex 3 次、Claude 4 次，都没有被控端树里的进程弹窗，结束会话后 0 残留（见"Windows 上的对话"一节）。
- [x] 服务模式 chat-runner：代码与单测完成（分支 `feat/host-chat-service`），**真机服务模式未验证**，步骤见下一小节。
- [ ] 服务模式真机：按下一小节的步骤 1~8 走一遍（要管理员终端）。
- [ ] Claude 真机端到端：要先在被控端重新登录 Claude（本机 token 已过期）。
- [ ] DeepSeek Harness 真机端到端：装好并配好 key 后跑 `tools/chat-smoke.py --quiet --real deepseek=<dsh 路径> --model deepseek-v4-flash`。
- [x] Codex / Claude 的运行中插话（见上一小节）。
- [ ] iOS 只读会话视图；可选的 `kind:"pty"` 原始终端。
- [ ] 与 `feat/host-tray` 合流（不论哪个先合进 main，后合的那个 rebase 时做）：
  - 被控端：把 `chat`、`set_chat_enabled` 加进 `HostRequest::OPS` 和 `op()`；
  - 桌面端：设置页的智能体开关先用 `can("chat")` 判断，再调用 `local_host_chat` / `set_local_host_chat`，并改成经 `host_local::request` 发送。这样旧被控端会统一显示"被控端版本较旧"和"更新被控端…"，不会再把 `unknown variant` 原样露给用户。（托盘分支 50bbbe3 已经有这套机制。）
- [ ] 合入 PR #18（`[network] quiet = true`）之后：`tools/chat-smoke.py` 给隔离被控端和控制端的 home 都写 `[network] quiet = true`，之后才能再跑。在公司网络里，隔离 home 默认会发 mDNS 组播和 UPnP 探测，容易被 IT 标记。本分支在那之前不起任何测试实例。
- [ ] 是否向 MCP 客户端开放 `chat_send` / `chat_read`（"AI 指挥 AI"），等用户决定。

### 服务模式：用户会话里的 chat-runner（2026-10-21，分支 `feat/host-chat-service`，基于 `feat/host-chat`）

问题：服务模式的被控端是 SYSTEM，智能体必须以登录用户身份运行，而 SYSTEM 执行用户目录里的程序等于本地提权；另外 Win11 锁屏时服务会杀掉旧会话里的 hostw、在新会话里重拉，进程内的智能体会跟着死。

**切分点在适配器层**：被控端（SYSTEM）保留会话、转录与编号、排队、审计和所有限制；runner（用户）只做"执行"——探测（`--version`、登录态、`~` 展开、工作目录检查）和 `adapters::launch` 跑智能体。`ChatManager` 多了 `Backend::{Local, Runner}`，driver 不变，只是 `LiveHandle` 的命令走管道、`Out` 从管道回来。

- **拉起**（`chat/runner_win.rs`）：`WTSEnumerateSessionsW` 找安装者（`--owner-sid`）的会话，Active 优先、Disconnected 也算（Win11 锁屏后用户会话就是 Disc）；`WTSQueryUserToken` 拿该登录的令牌（UAC 管理员拿到的是过滤令牌，智能体不提权）；`CreateEnvironmentBlock` 后追加密钥变量；`CreateProcessAsUserW` 拉 `ai-remote-hostw --home <配置目录> --chat-runner`，`CREATE_BREAKAWAY_FROM_JOB`（服务给 hostw 的 job 允许 breakaway；不允许时退回普通启动并告警，代价只是 hostw 重启时丢智能体），不继承任何句柄。懒启动：第一次要用（runtimes / start）才拉。
- **认证**（`chat/runner.rs`）：管道 `\\.\pipe\ai-remote-chat-runner-<id>` 用 `Listener::bind_shared(SYSTEM + 安装者 SID)`，但 ACL 只是第一道。双方各出 nonce，用 32 字节密钥做 blake3 keyed MAC 双向证明（`handshake_host` / `handshake_runner`）：抢注管道名的进程拿不到 runner 的信任、也让不了用户的智能体跑；没密钥的客户端连上也被拒。密钥由被控端生成、存 `host/chat-runner.key`（重启后的 hostw 要用它认回仍在跑的 runner），经 runner 的环境块传入，runner 启动第一件事就把它从自己环境里删掉；`proc.rs` 的清洗前缀也加了 `AI_REMOTE_CHAT_RUNNER`，智能体看不到。没有做"继承句柄"：要用 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 才能只继承一个句柄，而环境块只有该用户和管理员能读，同一用户本来就不是边界。
- **重启续接**：runner 不在 hostw 的 job 里，hostw 被杀它还活着，每 250 ms 重连。它给每个智能体的输出编号（`n`）并缓存到被确认为止（上限 16 MiB，超了就不读智能体输出，让智能体在管道上等）。被控端每 0.5 s 或轮次结束时让 driver 处理 `Out::Mark(n)`：**先**写进 `meta.json` 的 `runner_seq`，**再**发 ack，runner 才丢弃。重启后的被控端在 `load` 里把带 `runner_live` 的会话挂起不启动任何东西（`adopting`），等 runner 3 s；`lives` 列出还在跑的，逐个 `adopt(after = runner_seq)`，runner 补发之后的输出，会话继续（新的 seq 接着旧的编、不跳号），发一条 `reattached` 通知；进行中的轮次从 `meta.turn`（只存 id、发送者、哈希、字数、开始时间，不存正文）恢复。runner 里没人认领的智能体被停掉；runner 不在了，会话按原来的"被控端重启"处理（停掉遗留进程、轮次失败、parked）。
- **交接时丢的消息**：被控端发出提示词后立刻被杀，runner 可能没收到。adopt 应答带 `busy`（提示词进去了、轮次没结束）和 `ending`（补发里有轮次结束）；两者都否 → 轮次判失败（"消息没送到，请重发"），不会挂 30 分钟。这是单测第一版跑出来的真实竞态。
- **runner 何时退出**：被控端正常停止（`stop-host`）时 `ChatManager::close` 让它停掉智能体后退出；没有被控端连着超过 2 分钟（无智能体）/ 15 分钟（有智能体）自己退出。`sc stop` 杀掉 hostw 不算正常停止，智能体最多再跑 15 分钟。用户注销时 runner 随会话结束。
- 日志：`host/chat-runner.log`（runner 以用户身份写，和 SYSTEM 的 host.log 分开）。

**实测 / 未测**

- Mac：`cargo test -p ar-host --lib chat` 32 项，连跑 3 次全过；新增：握手（同钥通过、异钥双向拒绝）、密钥持久化、**真实 Unix socket 上的重启续接**（被控端放在独立 tokio runtime 里，`shutdown_background` 模拟被杀；智能体在被控端不在时作答，新被控端接回后收到这条回答、`seq` 连续不跳号、回答来自同一个进程 pid、之后的轮次仍是同一进程、runner 只被拉起过一次、`close` 后智能体进程退出）、runner 不在时会话变 parked 且不为了"看看"去拉 runner。全工作区 clippy `-D warnings` 无警告、`cargo test --workspace` 无失败。
- Windows（win-dev，Windows 11，debug，用户态 SSH 会话）：`cargo clippy --workspace --all-targets -D warnings` 无警告，`cargo test --workspace` 全过（ar-host 260 项）。其中 `runner_win` 两项是真 Win32 调用：取本进程令牌的 SID 字符串、`CreateEnvironmentBlock` 后追加变量得到双 NUL 结尾的块；普通用户身份下 `WTSQueryUserToken` 对每个会话都失败，所以找账号会给出"未登录"。重启续接那个端到端测试原来用 sh 假智能体、只在 Unix 上跑（2026-10-22 起两平台都跑，见下一条和"Windows 补测"一节）。
- 2026-10-22 补（分支 `chore/runner-verify`）：runner 的端到端测试改为两平台都跑——Windows 上替身是 PowerShell 脚本、端点是真的命名管道；
  新增"运行中插话经管道送达并并入当前轮次"（替身像 Claude Code 一样回显两行，走 `Steered`，`turns` 仍是 1）。Mac 连跑 2 次、
  win-dev 连跑 3 次，6 项全过（握手、密钥、重启续接、插话、图片、runner 不在）；win-dev 上 clippy `-D warnings` 无警告。
  新测试在 macOS CI 上抓到一个真 bug：runner 对每个请求都 `tokio::spawn`，同一智能体的提示词和插话可能乱序写进智能体
  （慢机上插话先到，被当成下一轮）。现在 `cmd` / `ack` 在读循环里按序处理，只有探测、启动、关闭这类慢请求并发。修后 Mac 连跑 15 次全过。
  这些仍是用户态进程内的测试，SYSTEM 拉起 runner 那一段只能真机验证。
- **未测（需要维护者在管理员终端操作，agent 不装服务）**：`WTSQueryUserToken` / `CreateProcessAsUserW` 以 SYSTEM 身份真正拉起 runner、breakaway 是否生效、锁屏切会话后续接、真实智能体在 runner 下的表现。

**真机结果（2026-10-22，Windows 11 开发机，服务 `ai-remote-host-cc166525` 跑 main 8eeabb6 + PR #29 的 release，
Mac 控制端 CLI，Mac 对它开着 bypass）**：没有换服务程序（另一个会话当天已换成带 runner 的构建），第 1、8 步免了。
- 第 2 步：会话 3 里 SYSTEM 被控端 + 用户 `LILITHGAMES\sunjiashuai` 的 runner（`--chat-runner`），`chat-runner.log` 有
  `connected to the host`，`host.log` 有 `started the agent runner in the user's session session=3`；`chat list` 里 claude 2.1.293 /
  codex 0.162.1 显示已登录、路径是用户 npm 目录下的 `.cmd`（SYSTEM 看不到这些登录态）。
- 第 3 步：`chat run codex "Run whoami"` → `lilithgames\sunjiashuai`，13.8 s；runner 下整棵进程树（cmd → node → codex.exe → 它拉起的
  pwsh / python / node_repl / MCP 服务）都属于该用户，`MainWindowHandle` 全是 0。
- 第 4 步（锁屏续接）：这台机器的策略禁用了"锁定"（Ctrl+Alt+Del 菜单里没有，SYSTEM 调 `LockWorkStation` 也无效），改用菜单里的
  "切换用户"：控制台 3 → 4，用户会话 3 变"断开"，服务在会话 4 重拉被控端，runner pid 不变，会话出 `reattached`（那一轮已在切换前
  18 s 结束，所以是空闲时接回）。**轮次进行中**：第 3 轮执行 `Start-Sleep -Seconds 90` 时用提权 agent 结束会话 4 里的被控端
  （Win11 锁屏时服务也是这样结束它的；提权 SSH 会话 `taskkill` 结束不了 SYSTEM 进程，拒绝访问）：1 s 后新被控端出 `reattached`，
  sleep 跑满 90 s，轮次 `completed`（"finished"），seq 43~57 连续，没有"host restarted during this turn"。各 1 次。
- 第 7 步：同一用户不带密钥连 runner 管道，在证明这一步得到 `{"ok":false}`，`host.log` 记
  `refused a connection to the agent runner endpoint: the runner did not prove it holds the key`；真 runner 不受影响。另一个本机用户的
  ACL 拒绝**没测**（要新建账号）。
- **没做**：第 6 步（`stop-host` 后 runner 随之退出，要人在管理员终端 `sc start` 拉回服务）；第 5 步只看了没有挂住的轮次。
  做完第 4 步控制台停在会话 4 的登录界面，要人解锁（skill win-dev-unlock）。

**真机验证步骤**（Windows 开发机。标"人"的步骤要用户在场或在管理员终端里做，agent 不代做；`<host>` 是 Mac 上已配对的这台开发机）：

0. 现状与前提（2026-10-22 查）：开发机上已经有一个真实服务 `ai-remote-host-cc166525` 在跑，配置目录
   `C:\Users\sunjiashuai\AppData\Local\ai-remote\data`，安静模式，binPath 指向 `ai-remote-lan\target\release` 的构建（属于
   `feat/lan-direct` 那边）。验证要把这个服务的程序换成含 runner 的构建，**换之前先征得用户同意**，并记下原 binPath，验证完再换回。
   验证用的构建已经编好：`C:\Users\sunjiashuai\ai-remote-chat-svc\target\release\`（分支 `chore/runner-verify` = main + runner 测试补强，
   含 PR #18 的安静模式）。在公司网络里另起测试用的被控端或控制端，都要在它的 home 下 `host/`、`controller/` 各放一份写有
   `[network] quiet = true` 的 `ai-remote.toml`；本流程只换现有服务的程序，不另起实例。
1. （人，管理员终端）记下原配置后换程序：
   `sc qc ai-remote-host-cc166525`（记下 BINARY_PATH_NAME），然后
   `C:\Users\sunjiashuai\ai-remote-chat-svc\target\release\ai-remote.exe --home C:\Users\sunjiashuai\AppData\Local\ai-remote\data host install --service --dry-run`，
   确认计划只改这一个服务的 binPath 后去掉 `--dry-run` 再执行。`... host status` 应为 running。
2. 探测以用户身份进行：Mac 上 `ai-remote chat list --host <host>`，应列出开发机上装的 codex / claude 等，`can_start: true`，版本和登录态与用户
   在自己终端里看到的一致。开发机上 `tasklist /v /fi "imagename eq ai-remote-hostw.exe"` 应有两个进程：`NT AUTHORITY\SYSTEM` 的被控端，
   和登录用户的 runner（命令行带 `--chat-runner`）。`<配置目录>\host\chat-runner.log` 有 `connected to the host`。
3. （人：会花真实智能体的额度）`ai-remote chat run --host <host> codex <用户目录下某个文件夹> "列出当前目录"`，或在桌面端「智能体」标签里做：
   轮次完成；任务管理器里 codex 进程的用户是登录用户、不是 SYSTEM，没有弹出控制台窗口。再发一条带图片的消息（桌面端拖一张图）、
   一条运行中插话，确认都送达。
4. （人）**锁屏续接**：发一个耗时的任务（例如"先 sleep 60 再回复 done"），轮次运行中按 Win+L。服务日志（`host\host.log`）应出现 console
   session 切换和新会话里重拉被控端；runner 的 pid 不变；控制端重新打开对话后看到 `reattached` 通知，60 s 后收到 done，轮次 completed，
   没有"the host restarted during this turn"。用 skill win-dev-unlock 解锁后再发一条，`chat sessions` 里智能体 pid 不变。
5. 交接时丢消息：手动难复现，单测已覆盖；真机上只看有没有挂住不结束的轮次。
6. （人，会停掉真实服务）`ai-remote stop-host`：服务停止，runner 和智能体进程都退出（`chat-runner.log` 有
   `the host told the agent runner to exit`）。随后 `sc start ai-remote-host-cc166525`，再 `chat list` 会拉起新的 runner。
7. 安全检查：以另一个本地普通用户连 `\\.\pipe\ai-remote-chat-runner-<配置 id>` 应被 ACL 拒；同一用户下不带密钥连上去，`host.log` 里应有
   `refused a connection to the agent runner endpoint`。
8. （人，管理员终端）恢复：用第 1 步记下的原程序路径执行它自己的 `ai-remote.exe ... host install --service`（或经用户同意后
   `sc config ai-remote-host-cc166525 binPath= "<原值>"`），再 `sc stop` / `sc start` 让原构建跑起来。**不要** `host uninstall`：
   那会删掉真实服务。

## 网络设置页重做：普通用户看得懂（2026-10-21，同在 `feat/lan-direct`）

用户要求：所有网络相关的设置都能在界面里操作、普通用户知道是什么意思，局域网模式和局域网发现的开关要友好，e2e 跑通再准备上线。

- 设置 > 网络：**连接方式**是两张单选卡（自动〔推荐〕/ 仅限局域网，用被控端级别那种 `.radio` 行），**可选功能**两个
  `.switch` 开关（在局域网里自动发现设备 = `lan_discovery`；让路由器开放端口（UPnP）= `port_mapping`，仅限局域网时隐藏）；
  仅限局域网时显示**这台电脑的地址**（只显示别的设备能用的：去掉回环、`fe80::`、`169.254.`，IPv4 在前，带复制按钮）和
  **要连接的电脑**（每台一行，修改 → 输入框，先在前端校验是 IP 再提交）。`quiet = true` 时两个开关变灰并写明原因。
  文案都按"能做什么、代价是什么"写，避免 mDNS / NAT 这类术语打头。
- 后端改成一个 `update_network(mode?, lan_discovery?, port_mapping?)`，同时写被控端和控制端两份配置，打开开关前
  `ar_core::config::ensure_not_quiet` 检查锁；命令行 `ai-remote network [mode auto|lan | discovery on|off | upnp on|off]` 走同一套。
- 修了一个竞态：改设置后重启控制端代理，紧接着的请求会撞上还没退出完的旧代理（"daemon closed the connection without
  answering"）。`ar_controller::restart_agent` 发 `shutdown` 后等到控制通道不再应答（最多 10 s）才返回，app 和命令行共用。
- 测试：`tools/network-ui.test.mjs`（地址过滤与输入校验）；`tools/network-ui-smoke.py`（camofox REST 驱动演示页，17 项，
  截图在 `target/network-ui`）；`tools/network-e2e.py`（一台机器上隔离的被控端和控制端，真实 QUIC，16 项：默认安静、两端切
  仅限局域网、没有局域网发现时不给配对码只给票据、配对、调用、doctor `lan`、被控端零 TCP 连接、换端口后 20 s 拨号超时、`hosts address` 后 0.05 s 恢复、quiet 锁拒绝）。
  macOS 和 Windows 开发机（debug、隔离目录）各跑一遍都 15 项全过（合入配对码前；合入后 Mac 16 项全过）。Windows 上要注意：子进程输出按 UTF-8 读（默认会按 GBK 解码失败）；
  从 SSH 会话起的被控端没有桌面，远程 `doctor` 截屏类检查 fail、整体退出码 1，脚本只看 `lan` / `network` 两项。
  仓库里原来的 `tools/desktop-ui-smoke.py` 依赖的 camofox 命令行（另一个分支的包）本机没有，所以新脚本直接用 REST。
- 坑：macOS 临时目录太长，控制端代理的 Unix 套接字路径会超长（`SUN_LEN`），e2e 的临时目录放 `/tmp`。
- 合 main 带进来的配对码（#17）自己建一个临时 iroh endpoint，不经过 mcpmesh：在 #18 里让它按同样的设置走（不做端口映射、
  DNS 回退只在系统没有 DNS 时用、有自建地址查询服务器就用它）；在 #19 里仅限局域网时关掉中继和地址查询，只能靠局域网发现找到。
  所以仅限局域网又没开局域网发现时不给配对码（`Mesh::pair_code_findable`），被控端在 `code_reason = "lan_discovery_off"` 里
  说明，配对窗口提示用票据或去 设置 > 网络 打开；控制端输配对码立即报同样的原因，不干等拨号超时。

## 仅局域网（内网直连）模式（2026-10-21，分支 `feat/lan-direct`，基于 PR #18）

用户要在公司里只走内网：不连中继、不连地址查询服务，网络上只剩两台设备之间的连接。

做法（读代码确认 mcpmesh 已具备底子）：mcpmesh 的 `relay_mode = "disabled"`（原来只给本机测试用的"hermetic"模式）没有中继、
没有地址查询，中继表为空时 iroh 连 net_report 和端口映射都不做；拨号用每个配对设备存的 `last_addr`（配对时从票据记下，
之后每条活着的连接都会刷新，见 `daemon/dial_hint.rs`），`Node::connect_protocol` 每次拨号都带上它，ai-remote 的所有通道都走这条路。

- `[network] mode = "lan"`（`auto` 默认）：写成 `relay_mode = "disabled"`，忽略 `relays` / `address_lookup`；被控端没写 `port`
  时固定 UDP 47800（`ar_core::config::LAN_HOST_PORT`），控制端端口仍随机（同一台机器上被控端和控制端不抢端口）。
  `NetworkOptions::from_config` 因此又带上了角色参数。
- 第五个 vendored 补丁：`Node::peer_addr` / `set_peer_addr`（只收可拨的 IP 地址，绝不新建配对条目）。往上是
  `Mesh::peer_addrs` / `set_peer_addrs`、控制端请求 `set_host_address`、`hosts` 列表带 `addresses`、
  CLI `ai-remote hosts address <host> [<地址>]`（不写端口补 47800，地址解析在 `ar_core::config::parse_host_addresses`）。
- 被控端 `status` 带 `lan_direct` 和本机地址；`doctor` 在仅局域网模式下用 `lan` 检查代替 `relay`（列出控制端该用的地址，
  固定端口被占、改用随机端口时报 warn），`p2p` 跳过（没有中继就没有网络探测）。
- 桌面端 设置 > 网络：连接方式（自动 / 仅局域网，写被控端和控制端两份配置并重启控制端代理）；仅局域网时显示本机被控端地址、
  每台已配对电脑的地址和手动修改框；UPnP 一项在仅局域网时隐藏（没有中继时 iroh 根本不做端口映射）。
- 测试：`crates/ar-net/tests/lan_direct.rs` 起两个仅局域网节点：配对、拨号回显；被控端换端口后，新起的控制端拿旧地址连不上，
  `set_peer_addrs` 设成新地址后又能连上（本机 14 s）。注意 iroh 会在内存里记住活连接的路径，所以"连不上"要用新起的节点验证。

限制：离开这个网络就连不上；被控端换 IP 要在每个控制端上改一次地址；已经配对的设备记的是切换前的随机端口，切到仅局域网后
第一次要手动设一次地址（或重新配对）。

实测（2026-10-21，公司网络，release 被控端在 Windows 开发机、debug 控制端在 macOS，都是隔离的 `--home`、`mode = "lan"`）：
- 配对 < 1 s，控制端存的地址是 `192.168.1.20:47800`；`system_info` 往返 0.05 s，`hosts` 报 `direct (P2P)`。
- 被控端重启到 47801：控制端代理一直开着时**自己找回来了**（被控端按它存的控制端地址连过来，iroh 记下了新路径），存的
  提示还是 47800。两边都换（控制端代理重启、被控端换 47802）后：旧地址 20 s 拨号超时；`hosts address lan-test
  192.168.1.20:47802` 之后 0.04 s 连上。
- Windows ETW（`Microsoft-Windows-Kernel-Network`，按 PID 记 UDP 发送和 TCP 建连）120 s：被控端 245 个 UDP 全发往控制端
  的 QUIC 端口，TCP 建连 0；同时段其他进程的 163 次 TCP 建连、Steam SSDP、Bonjour mDNS 都抓到了。
- macOS `tcpdump -i pktap,all -k`（全部 UDP + TCP SYN）120 s：控制端只和被控端互发 UDP（收 263、发 245），另向被控端
  WSL / Hyper-V 虚拟网卡的两个 `172.x` 地址各发 61 个（iroh 会试对方公布的每个地址；都是私有地址，
  不出公网，但会经网关）；TCP 0、5353 / 1900 / 5351 0、公网地址 0。
- 坑：Windows OpenSSH 断开会杀掉会话里起的进程，测试被控端要用 `Invoke-CimMethod Win32_Process Create` 起；
  PowerShell 里 `$Home` 是只读内置变量。控制端代理闲置 30 分钟会自己退出，换 pid，按进程统计时要以抓包时的 pid 为准。
- **两台真实机器已切到仅限局域网（2026-10-21，用户要求"直接切"）**，`quiet = true` 保留：Windows 开发机的服务改用本分支 release 版（`host install --service` 重装：停服务、改 binPath、给新程序加防火墙放行；这个版本没有 `host start`，用 `sc start` 起）；Mac 用 #14 + #18 + #19 打的包替换 `/Applications/ai-remote.app`（先删旧包再 `ditto`，然后 `launchctl kickstart -k`）。两边先用新 CLI
  `ai-remote network mode lan` 写配置，旧配对存的是随机端口，所以各自 `hosts address` 填一次对方的 `<ip>:47800`。结果：Mac → Windows `direct`、`info` 0.05 s；Windows → Mac `direct`、0.16 s；两边 `doctor` 的 `network`（安静、已锁定）和 `lan` 都是 ok。Mac 换包后屏幕录制 / 辅助功能授权失效（ad-hoc 签名变了），要机主在 app 的"本设备"页重新允许。

## 安静模式：受管理网络里少发流量（2026-10-21，分支 `feat/quiet-network`）

起因：用户担心在公司网络里被 IT 标记，问被控端是不是一直在扫端口、发请求。结论（读代码，iroh 1.3.0 / portmapper 0.19.3 /
swarm-discovery 0.6.4；本机只用 `lsof` 看了一次 socket，没有 sudo 抓包，频率都是**推导**）：

- ai-remote 自己的代码不扫端口、不扫地址、不检查更新、不上报遥测；桌面端 CSP 只允许本地 IPC。
- 后台流量全来自传输层 iroh：中继长连接（本机实测连着 `aps1-1.relay.n0.iroh.link`，TCP 443）、每 20–26 s 的 net_report
  探测、每 5 分钟的 pkarr 发布；**被控端默认 mDNS 约每秒一次组播并通告公网 IP**；**iroh 默认开着端口映射**，没有映射时
  每 20–26 s 发一次 UPnP SSDP 组播和 NAT-PMP/PCP；系统 DNS 失败后会转问 1.1.1.1 / 8.8.8.8 / 9.9.9.9。

做了什么（先按"默认不变、手动开启安静"做，同一天用户改主意：**默认就安静**，UPnP 放到界面设置里开）：

- vendored mcpmesh 第四个补丁：`[network].port_mapping`（false 时 `PortmapperConfig::Disabled`）和
  `[network].dns_fallback`（`deferred` 默认 / `if_system_empty` / `never`，非法值启动报错），mcpmesh 自己的默认都是 iroh 原行为；
  ai-remote 每次都显式写这两项。
- ai-remote 的默认（被控端、控制端一样）：`lan_discovery` 关（以前被控端 `on`、控制端 `resolve`）、`port_mapping` 关、
  `dns_fallback = "if_system_empty"`。`NetworkOptions::from_config` 去掉了按角色给默认值的参数。
- `[network] port_mapping = true` 打开端口映射；桌面端 设置 > 网络 的下拉框写的就是它（`ar_core::config::set_port_mapping`，
  用 toml_edit 保留注释，被控端和控制端两份配置一起写），写完给控制端代理发 `shutdown` 让它立即按新配置重启；本机被控端
  只在下次启动时生效（主干上还没有重启入口，PR #14 合并后可以接它的 `local_host_restart`）。
- `[network] quiet = true` 变成"锁"：不管 `lan_discovery`、`port_mapping` 和界面怎么设都关（界面下拉框置灰并说明原因）。
- `address_lookup = [...]`：自建 pkarr 服务器，和自建中继、`relay_fallback_n0 = false` 一起可以做到完全不碰 n0。
- `doctor` 的 `network` 检查报告实际打开了哪些；对称 NAT 时 `p2p` 的建议按状态给（没开就提示去 设置 > 网络 打开，安静锁住时
  不让人开）。
- 文档：`operations*.md` 第 11、12 节和 doctor 表格改成"默认关、按需开"，加了"网络上都有哪些流量"的表。

默认（安静）下配对过的设备照样能连（地址查询 + 打洞 + 中继）。代价：无外网局域网找不到对方；只能靠端口映射打通的 NAT
后面一直走中继（家里两端都是对称 NAT 时，到 设置 > 网络 打开端口映射）；公司 DNS 解析不了 `*.iroh.link` 时连不上。

注意：`NetworkSection` 是 `deny_unknown_fields`，**旧版本读到 `quiet` / `port_mapping` 会启动失败**。给机器写这两项之前先换新版本。

实测（2026-10-21，公司网络，本机 macOS 被控端+控制端、Windows 开发机被控端都开 `quiet = true`）：
- 连接：本机控制端连 Windows 被控端，`hosts` 报 `direct (P2P)`，只读工具（`ps`）立即返回。
- Windows：ETW `Microsoft-Windows-Kernel-Network`（按 PID 记录每次 UDP 发送）抓 120 s。被控端进程只发往对端控制端的
  QUIC 端口、4 台 n0 中继的 UDP 7842、以及对端的一个公网候选地址；发往 5353 / 1900 / 5351 的 0 个。同一时段别的进程
  （Steam 的 SSDP 12 次、Bonjour 的 mDNS 1 次）都被抓到了，说明方法本身能抓到这类包。
- macOS：`tcpdump -i pktap,all -k`（按进程标注）抓两次。控制端 7 分钟、被控端 148 s：只发往对端 QUIC 端口和 4 台中继的
  UDP 7842，发往 5353 / 1900 / 5351 的 0 个，也没有发往公共 DNS 的。同一时段系统的 mDNSResponder 有 mDNS 包被抓到。
  对照：同一天默认模式下，被控端是占着 `UDP *:5353` 的；安静模式下两端都不再绑定 5353。
- 没有在这台机器上抓默认模式的 SSDP 作对照（避免在公司网里制造流量），"默认模式每 20–26 s 一次 SSDP"仍是读代码得出的。
- 坑：macOS 上 pktap 的 tcpdump 收到 SIGTERM 不退出，脚本里要用 `kill -9`。

未测：公司 DNS 拦截 `iroh.link` 的情况。

## 对话侧栏照 Orca 重做（2026-10-21，分支 `claude/desktop-chat-ui-f97e8c`，基于 `feat/host-chat`）

用户看了旧对话界面，要求直接照 Orca（stablyai/orca，MIT）的 native chat 搬，做到接近 Claude / Codex 原生 App。只改了桌面端和前端测试，被控端没动（另一个会话在做插话）。来源与许可证见仓库根的 `THIRD_PARTY_NOTICES.md`；每个搬来的组件文件头注明对应的 Orca 组件。

### 怎么搬的

- Orca 的 native-chat 是 108 个组件、5 万多行，依赖它自己的 store、i18n、tiptap、Tailwind + shadcn，整块搬不动。做法：**布局、间距、配色比例、折叠方式和文案语气照抄，组件按我们的事件重写**。
- 转换层在 `lib/chat.ts` 的 `turnRows`：一轮投影成若干"行"。
  - 连续的工具调用折成一个"运行组"（run），用一句话概括："运行了 2 条命令，读了 1 个文件"；进行中改成"正在读 1 个文件，运行 2 条命令"。
  - 夹在两个工具之间的思考算在组里；其他位置的思考单独一行（"思考了 4 s"）。
  - 编辑也算在运行组里（"编辑了 1 个文件 和 运行了 1 条命令 +3 −2"），展开后是 diff 卡片。最初做成了独立的 diff 卡片行，实机对照 Orca 后改成这样，见下面"Orca 实机对照"。
  - Claude 的 Task 工具（`kind: think` 且标题以 Task/Agent 开头）是子智能体行；计划、提问、通知、插话各自一行。
  - 只有一个工具的运行组直接画成那一行工具（Orca 也是"一条命令就显示命令本身"）。
- 组件在 `ui/chat/`：`Markdown.tsx`（Markdown、代码块、复制按钮、高亮）、`Rows.tsx`（各类行）、`Composer.tsx`（输入框、排队卡片、上下文用量环）、`Transcript.tsx`（消息列表、跳到最新）。`AgentChat.tsx` 只剩启动表单、会话选择和连接。
- 样式仍只在 `styles/native.css`（第 6b 节），没有引入 Tailwind。颜色按 Orca 的比例用我们的令牌混出来：正文 88% 前景、代码底 3.5%、行内代码 6%、用户气泡 6%、发丝线 8%。

### 依赖：一个都没加

- **Markdown**：自己写的 `lib/markdown.ts`（约 360 行，GFM 子集：标题、段落、围栏代码、引用、嵌套列表、任务列表、表格、分隔线、行内代码、粗斜体、删除线、链接）。
  - 没用 marked / react-markdown：前者产出 HTML，要再加 sanitize；后者连同 remark/rehype 要拉进几十个包。
  - 自己写的版本只产出一棵文本树，React 只渲染文本节点，原始 HTML 永远是文字。这和旧界面"从不渲染 HTML"的承诺一致。
  - 链接只认 http(s) / mailto，点击复制地址，不在应用里跳转（跳了页面就离开对话栏了）。
- **语法高亮**：自己写的 `lib/highlight.ts`（约 210 行），覆盖智能体最常输出的十几种语言（JS/TS、Python、Rust、Go、C 系、Shell、PowerShell、JSON、TOML、YAML、SQL、CSS、HTML、diff）。
  - 没用 shiki：它 4.x 带 WASM 引擎和语法包，体积是 MB 级。
  - 只是分词着色，猜错只是颜色不对；超过 6 万字符不着色。
- **diff**：`diffRows` 用 LCS 做真正的逐行 diff，变动前后各留 3 行上下文，两段改动之间折成"⋯ N 行未改动"（开头和结尾不标）。旧版只掐头去尾，中间两处小改会显示成整段删、整段加。单元格超过 400 万时退回旧做法。

### 和插话会话的协议约定（对方照这个实现）

- `caps.steer: bool`；`input.mode` 可取 `"queue" | "interrupt" | "steer"`。
  - 前端排队时不带 `mode` 字段，旧被控端照常读得懂。
- `user` 事件可带 `steer: true`，显示在正在跑的那一轮里，带"插话"标记。
  - 前端以"当前正在跑的轮"为准，不看事件里的 `turn`；被控端按当前轮编号或按下一轮编号都能正确显示。
- `notice` 码 `steer_queued`：前端用自己的文案"这个智能体不支持在运行中插话，已排队…"，不用被控端给的文字。
- `usage` 可带 `context_window`、`context_used`。两个数都有时，输入框右下角显示上下文用量环，超过 90% 变红。

### 交互

- **运行中输入**：
  - 智能体支持插话时，默认插话；输入框下方的芯片可以切成"排队"。不支持时芯片固定显示"排队"。
  - ⌥Enter 停止本轮并立即发送。Esc（输入框为空时）等于停止。
- **排队消息**显示成输入框上方的卡片，有三个操作：
  - 编辑：协议没有"替换"，实现为先撤回再重新排队，所以编辑过的消息会排到队尾；
  - 撤回：用现有的 `dequeue`；
  - 现在插进这一轮：只在支持插话时出现，实现为撤回后用 steer 发送。
- **其他输入**：↑/↓ 在光标位于开头或结尾时翻看本次运行里发过的消息；输入法组字时的 Enter 不会发送；发送后列表跟到底部。
- **提问卡片**：选项带编号，按数字键直接回答（焦点不在输入框时）；也可以输入别的回答。回答后卡片收成一行"已回答：…"。
- **被停止的轮**：显示"你停止了这一轮。[继续]"。"继续"发送"从刚才停下的地方继续，先确认上一步有没有做完"（Orca 的 Continue 也是先让智能体核对）。被停止打断的工具记为"已停止"，不算失败。
- **长对话**：
  - 只挂载最近 30 轮，顶上有"显示更早的 N 轮"；
  - 倒数第 3 轮及更早的轮用 `content-visibility: auto`，在屏幕外时跳过布局和绘制；
  - 每轮按对象 memo，流式输出时只重渲染正在跑的那一轮；
  - 读者往上翻时不再自动跟随，出现"跳到最新"按钮。
- **单复数**：所有计数文案都拆成 `_one` / `_many` 两个键（中文两个一样），修掉了 "1 steps"。测试会检查每个计数键两种语言都有、英文单数不出现 `{n}`。

### 实测

- **门槛检查**：`tsc --noEmit` 通过；`node --test tools/*.test.mjs` 80 通过、0 失败、2 跳过。
  - 新增 `tools/chat-render.test.mjs` 共 7 项，覆盖 Markdown、高亮和单复数。
  - `tools/chat-state.test.mjs` 从 9 项扩到 18 项，覆盖行投影、运行组、子智能体、插话（两种 turn 编号）、steer/queue/interrupt、排队编辑、历史、上下文用量、停止后继续、diff 统计、单复数。
  - 没改 src-tauri，所以没跑 clippy。
- **纯逻辑性能**（node，本机 M 系列 Mac，单次）：
  - 200 轮、4.54 万个事件（每轮 12 个工具 + 200 个流式片段）折叠一共 25 ms，平均每个事件 0.56 µs；
  - 200 轮全部投影成行用了 2.6 ms；
  - 2.7 KB 的 Markdown 解析一次 0.11 ms；
  - 浏览器里的渲染耗时没量。
- **演示模式截图**（camofox，`npx vite` + 演示后端，视口 2319 宽，侧栏 560 px）在 `/tmp/chat-ui-v2/`，没进仓库：
  - 空闲、展开的工具和 diff、插话加提问、流式输出、排队编辑、停止后继续、深色英文、长消息折叠，共 9 张。
  - 第一轮截图过程中发现并修掉了：
    - 全局 `.tgt` / `.hd` / `.live` / `.wrap` 类名污染了新组件，新组件改用 `obj` / `rh` / `is-live` / `wrapped`；
    - 提问卡片在提前 return 之前少调了一个 hook，回答后整页崩溃；
    - shell 高亮把 `export.py` 的 `export` 当成关键字。
- **和 Orca 对照**：见下一小节。

### Orca 实机对照（2026-10-21）

- **怎么跑的**：
  - 从官方 GitHub release 下了 Orca 1.4.237（`Orca-1.4.237-arm64-mac.zip`，Developer ID 签名、已公证），放在 `/tmp/orca-app`，用 `--user-data-dir=/tmp/orca-profile --remote-debugging-port=9333` 启动；
  - 打开设置 → 实验性 → Chat UI；截图、点击都走 CDP（脚本在 `/tmp/orca-cdp/`），不占用屏幕。
- **没能现场跑任务**：本机 Codex 0.137 用 ChatGPT 账号时 gpt-5.5 / 5.4 / 5.3-codex / 6.1-sol 全部被拒（400），Claude 的 OAuth token 过期，本机也没装 OpenCode。
  - 改用 Orca 的"智能体会话历史 → 在新聊天中恢复"，把本机一个已结束的 Claude Code 会话在 native chat 里打开。只渲染历史，没有发送任何消息。
- **截图**：`/tmp/chat-ui-v2/orca-*.png`（Orca）和 `10-after-orca-run-with-diff.png`（改后的我们），没进仓库。
- **对照后改了的**：
  - 编辑并进运行组：Orca 写成"编辑 2 文件 和 运行 1 个命令 ✓"，展开后每个文件一行"编辑了 X.swift +7 −6"，再展开是 diff。我们原来把每次编辑做成独立的卡片行，一轮里改的文件多了就很长。现在组标题后另加总的 +N −M。
  - 句子最后一项用"和 / and"连接。
  - 命令只显示命令本身，不再写 "Bash"。
  - 失败数从红字改成灰色的"· 1 个失败"；运行组标题的箭头只在悬停时出现。
  - diff 行自动换行（原来是横向滚动），复制按钮常显，开头和结尾不再标"N 行未改动"。
  - "跳到最新"改为"跳到最新消息"，和 Orca 的中文一致。
- **对照后确认一致的**：
  - 正文和运行组的层次：运行组用灰字、紧贴在上一段正文下面，展开后左边一条竖线、每行图标 + 目标 + 右侧箭头；
  - 行内代码的描边底色；
  - "跳到最新消息"浮在输入框上方居中；
  - 输入框是一个圆角大框，按钮在文字下方一行。
- **仍不同的**：
  - Orca 正文 14 px、内容列最宽约 46 rem 居中；我们在 560 px 侧栏里用 13 px。
  - Orca 的 diff 有行号槽：Claude 的 Edit 带了真实行号。我们的事件只有改动前后的文本片段，没有行号，所以不显示行号槽（Orca 在不知道行号时也这样做）。
  - Orca 文件名可以点开文件；我们只能复制。

### 和 Orca 的差距（没做的）

（2026-10-22 输入侧补齐后更新，做完的打勾，做法见下面"对话输入侧补齐"一节。）

- [x] 发送状态："发送中… / 未送达 + 重新发送 / 丢弃"。
- [ ] 消息操作：没有"回退到这里"（Rewind）。
- [x] ⌘F 查找；[x] 选中回复里的文字引用到输入框。
- [x] `@` 文件；[x] `/` 命令与技能；[x] 图片粘贴、拖入、选择（只收图片，不收其他附件）。
- [ ] 富内容：没有 Mermaid 和内联图表。
- [ ] 文件路径：点了不能打开，只能复制（另一会话 chat-preview 做了预览弹层）。
- [x] 上下文用量环：点开是按类别的明细（只列智能体报告了的类别）。
- [ ] 窗口化：Orca 按行测量高度做真正的虚拟列表；这里是"只挂最近 30 轮 + content-visibility"，几千轮的极端长对话没测过。查找也只搜挂着的内容。
- [x] 子智能体：Claude 子智能体内部的工具调用带 `parent` 发出，折在子智能体行下面。子智能体自己的文字仍不显示（只显示它交回的总结）。Codex 的 `collabAgentToolCall` 没有拆内部步骤。
- [ ] 后台任务行、停靠在输入框上方的任务列表：没做；计划只在原位显示。
- 演示里的 `steer_queued` 提示：前端对不支持插话的智能体直接按排队发送，所以正常操作下看不到这条提示。它只在客户端发了 `mode: "steer"`、而被控端判定不支持时出现；渲染逻辑有单测覆盖，但截图里没有它。

### 下一步

- [x] 真实 Tauri 端到端（2026-10-22，另一会话在 `claude/desktop-chat-e2e` c271e66 做的，`feat/host-chat` 4be9e12 + 调试版 + 本机隔离被控端安静模式）：替身智能体 36 项 ×2 次全过；真实 Claude Code 2.1.307 13 项 ×1 次全过，插话 2 s 内带标记进同一轮。真实 Codex 账号对 gpt-5.5 报 400，提问卡片没在真实连接上测过。截图在 `/tmp/desktop-chat-e2e*/`。
  - 它报的问题已修：记住的会话在被控端不存在（被控端重启过）时，面板停在报错和发不出去的输入框 → 现在清掉记忆、回到会话列表并说明原因（`sessionGone`）；从记住的会话进来再点"其他会话"时父组件的 target 本来就是 null，原来不会重新渲染，现在由面板自己的状态驱动。
  - 窗口不可见时 rAF 不触发，事件会积到窗口可见才显示 → 加了 100 ms 定时器兜底；运行计时一直显示 "0 ms" → 计时器在轮次开始时就取当前时间，不再等第一次 1 s 的跳动。这两处没有在不可见窗口里复测。
- [x] Windows（WebView2）上看一遍：字体、滚动跟随、`content-visibility`、`mask-image`（见"Windows 上的对话"一节；修了对话区滚动条和代码里中文的字体）。
- [ ] iPhone 端照桌面端跟进。转换层（`turnRows`、`runSummary`、`diffRows`）是纯函数，Swift 可以照着移植。

## 对话输入侧补齐（2026-10-22，分支 `claude/chat-input-8764a1`，基于 `feat/host-chat` 8b2b63a，已合入 `claude/dsh-agy-7f8142`、`claude/chat-preview-97901c`、`claude/chat-folder-5ebe7f`，最后合入改写过历史的 `feat/host-chat` 295aa2f，合入后替身冒烟 35 项、桌面 e2e 67 项各再跑 1 次全过）

用户要求体验接近 Claude / Codex 原生 App，把"和 Orca 的差距"里输入这一侧补齐。参考 Orca（`/tmp/orca-src`，MIT）的
NativeChatAutocompleteMenus、NativeChatFindBar、NativeChatSelectionQuote、NativeChatContextUsageRing、subagent sections，
只借布局和行为，组件按我们的事件重写（文件头注明出处）。没有加新依赖。

### 做了什么（每一步一个提交）

1. **只留一个发送按钮**：`feat/host-chat` 的 e3c0410 已做完（和 iPhone 端一致，插话消息不加标记、保留 `data-steer`）。
   用户中途又要求：输入框改成一行，**最左边一个 `+`**（加图片），文字在中间，停止 / 发送在右边，提示和上下文环放到框下面
   （照 Claude App 的截图）。原来的图片图标去掉了。
2. **发送状态**：被控端对每条 `input` 回 `{"t":"ack","input_id"}`，拒绝时回带 `input_id` 的 `error`，hello 里 `acks: true`；
   队列满被丢掉的消息发带 `input_id` 的 `queue_full` 通知。前端（`lib/chat.ts` 的 `Outgoing` / `settleOutgoing`）把自己发的
   消息放在对话末尾显示"发送中…"，收到 `user` 事件或出现在队列里就消失；被拒、连接断开、15 s 没有 ack 时显示"未送达"和
   "重新发送 / 丢弃"。重发：连接问题用原 id（被控端按 id 去重，不会跑两次），被拒的换新 id。
3. **`/` 命令与技能**：新事件 `commands`（`name`、`description`、`kind` command|skill、`insert`、`hint`），列表变了才写进
   对话记录，存进会话 meta；被控端还按"智能体 + 目录"记住最近的列表，新会话在智能体启动前就能用。
   - Claude Code：启动时写一条 `control_request` `initialize`，回应里有全部命令和技能及说明（本机 2.1.307 实测 200 条，
     在任何消息之前就回）；第一条 `init` 的 `skills` 说明哪些是技能，所以列表等 `init` 到了才发（否则每次启动发两遍）。
     旧版本不回 `initialize` 时用 `init` 的 `slash_commands`（只有名字）。
   - Codex：线程起来后调 `skills/list`（方法名和返回结构来自 0.163 的 `generate-json-schema`），技能以 `$name` 列出；
     消息里写了 `$name` 时，`turn/start` / `turn/steer` 的 input 里同时带 `{"type":"skill","name","path"}`。Codex TUI 的
     内置斜杠命令（/compact、/review 等）在 app-server 里不是文字命令，没有列。
   - ACP：`available_commands_update`（`session/load` 回放期间也收）。
   - **坑**：第一版把列表放进 hello，win-dev 上 Claude 的 208 条命令超过了控制端读应答行的 64 KiB 上限，attach 直接失败
     （"oversized reply"）。改成 hello 之后单独一行不编号的 `{"t":"commands",...}`，旧控制端也不受影响。
   - 输入框：消息开头输入 `/` 弹出（命令在前、技能在后、分组标题），↑↓、Enter / Tab 选中、Esc 关；选中的命令原样作为文字发。
4. **`@` 文件**：chat/1 新命令 `{"files":{"rid","query","limit"}}`，回 `{"t":"files","rid","files","truncated","source"}`。
   被控端（`chat/files.rs`）在会话目录里跑 `git ls-files --cached --others --exclude-standard`，不是 git 工作树就向下遍历 6 层
   （跳过隐藏目录和 node_modules / target 等，不跟符号链接），最多保留 10 万个名字、缓存 10 s，在被控端按"每个词都包含"过滤，
   最多回 200 个。输入 `@` 后停顿 120 ms 再问；选中插入相对路径（有空格时加引号）。前端的请求 / 应答用 `rid` 配对（`requester`）。
5. **图片**：`input.attachments: [{name, mime, data(base64)}]`，PNG/JPEG/GIF/WebP，每条最多 4 张，每张解码后 ≤ 3.75 MiB
   （= base64 5 MiB，Anthropic API 的单图上限），被控端命令行上限因此从 1 MiB 调到 24 MiB。图片只在内存里随排队消息走，
   对话记录只写 `user.attachments` 的名字、类型、大小。各家：Claude 是文字前的 base64 image 块；Codex 是 data URL 的
   `image` 输入（实测能认出颜色）；ACP 在 `initialize` 的 `promptCapabilities.image` 为真时发 image 块，否则去掉图片并发
   `no_images` 通知，同时发新事件 `agent {images:false}`。`caps.images`：Claude、Codex 为真，DeepSeek Harness 为假，其他
   ACP 先按真、等智能体自己说。输入框：粘贴、拖进输入框、点 `+` 选文件；长边超过 2048 px 或超限时用 canvas 缩小重编码；
   拖进输入框的文件不会再触发推流画面的"传文件到被控端"（`stopPropagation`）。自己发的图片在本机消息气泡里显示缩略图，
   其他设备只看到文件名。
6. **查找 / 引用 / 用量明细**：⌘F / Ctrl+F（焦点在对话面板内时）或标题栏的放大镜打开查找条，用 CSS Custom Highlight API
   高亮（当前项 `priority = 1` 画在上面），Enter / Shift+Enter 跳转、Esc 关；只搜挂在页面上的文字（折叠内容和没挂载的旧轮次
   不搜）。在对话里选中文字出现"引用"按钮，点了以 Markdown 引用行插进输入框。上下文环点开是明细：当前占用 / 剩余，上一轮
   输入（其中读缓存、写缓存）、输出（其中推理）、费用；`usage` 事件新增 `cache_read_tokens`、`cache_write_tokens`、
   `reasoning_tokens`（Claude 取 `result.usage`，Codex 取 `tokenUsage.last`）；没报的类别不显示。
7. **子智能体内部步骤**：Claude 带 `parent_tool_use_id` 的 assistant 消息里的工具调用现在作为带 `parent` 的 `tool_call`
   发出（结果照常是 `tool_update`）；子智能体的文字和它的 TodoWrite 不当作主线的。前端 `turnRows` 把这些调用放进子智能体行
   （"2 步"，展开是各步，再下面是总结），父调用不是本轮子智能体的仍留在主线。
   - **`Rows.tsx` 只有两处最小改动**（chat-preview 会话在改消息行渲染）：`UserRow` 里显示附件（新增 `Attachments`），
     `AgentRow` 接收 `children`。
- 命令行：`ai-remote chat send … --image <文件>`（可重复，等被控端 ack，被拒时退出码 2 并说原因）、
  `ai-remote chat commands <会话>`、`ai-remote chat files <会话> [查询]`。协议写在 `docs/tools*.md` 5.15、`chat_channel.rs`
  文件头、`chat/event.rs`；`skills/ai-remote/SKILL.md` 7b、`docs/operations*.md` 4.1 表格同步了。

### 实测（2026-10-22）

- **门槛**（本机 macOS）：`cargo fmt --check`、`cargo clippy --workspace --all-targets --locked -D warnings` 通过；
  `cargo test --workspace --locked --no-fail-fast` 361 过 0 失败（ar-host chat 相关新增 13 项：会话里命令列表的保存与去重、
  新会话复用、图片进智能体且不进记录、`Image::parse` 限制、files 的遍历 / git 两条路、三个适配器的命令 / 图片 / 子智能体）；
  `tsc --noEmit` 通过；`node --test tools/*.test.mjs` 112 过 2 跳过（新增 `tools/chat-input.test.mjs` 15 项）。
  `stream::pointer::mac::tests::the_current_pointer_can_be_read` 在一次整跑里失败、单独重跑通过（已知会随光标变化失败，本分支没动）。
- **`tools/chat-smoke.py --quiet`**（隔离被控端和控制端都是安静模式）：
  - 本机替身：35 项全过（最后 2 次）。新增：命令列表含说明和技能、`@` 文件（git 工作树里不含被忽略的 node_modules、多词匹配）、
    图片完整到达智能体（替身回报 base64 长度）、超限图片被拒（退出码 2）、记录里只有名字和大小、命令列表在记录里只出现一次。
  - 本机真实 Codex 0.163（`--model gpt-6.1-sol`）：1 次全过。认出红色图片；`chat commands` 列出 136 个技能（含工作目录里放的
    `.codex/skills/pong-skill`），发 `$pong-skill please` 3.8 s 回 SKILLPONG；原有的打断、续上文、插话（数到 5）也过。
  - win-dev 真实 Claude Code（`D:\nodejs\node_global\claude.cmd`，被控端在终端前台运行，debug 构建）：最后 1 次 44 项全过。
    认出红色图片；列出 208 条命令；从列表里选 `/pong`（工作目录 `.claude/commands/pong.md`）2.4 s 回 SKILLPONG；插话 4.0 s 进
    同一轮、数到 5 停。之前两次失败分别是 hello 超过 64 KiB（已改协议）和 Windows 上 Popen 按 GBK 解码（脚本已改 UTF-8）。
- **`tools/desktop-chat-e2e.py`**（debug 桌面包 + 本机隔离被控端，窗口在屏幕外）：最后 3 次 67 项全过。新增覆盖：一行布局
  （`+` 在最左、发送在最右）、`/` 弹出（说明、技能分组、Tab 选中）、`@` 弹出（被控端真实列文件、Enter 插入路径）、粘贴图片
  （WKWebView 接受构造的 paste 事件）、发出后气泡里有缩略图且替身收到图片、送达后没有残留的"发送中"、子智能体"2 步"展开、
  ⌘F 查找 9 处并跳到第 2 处、Esc 关闭、选中引用、用量明细 ≥ 4 行。截图在 `/tmp/chat-input/`（10~17 号是新的），看过；
  没进仓库（推流画面是本机桌面）。
  - e2e 里踩到的：我给"发送中"的消息用了 `nt-out` 类名，和工具输出框撞了（样式也串了），改成 `nt-sending`；用量弹层的 `.bar`
    继承了输入框底栏的样式、`.hd` / `.row` 是全局类，都改了名；⌘F 原来先关再在下一帧打开，窗口不可见时没有动画帧，查找条
    打不开，改成直接打开并重新聚焦；`chat send` 等 ack 时把读到的命令列表行放回暂存队列、又从暂存队列读，死循环，改成等 ack
    只读流本身。
- 演示模式（camofox，`npx vite` + 演示后端）：`/`、`@`、图片、查找、用量弹层截图看过（Firefox 内核，和 WKWebView 不同）。

### 补测（2026-10-22 下午，从 main 8eeabb6 起的 `claude/chat-input-verify`）

- **真实 DeepSeek Harness 0.2.1**（`tools/chat-smoke.py --quiet --real deepseek=… --model deepseek-v4-flash`，本机，1 次 40 项全过）：
  任务、shell 工具、打断 0.0 s、续上文都过；发图片被被控端直接拒绝并说明"takes no images"（退出码 2）。
  它**不发 `available_commands_update`**（手动走 `initialize` + `session/new` 抓了 8 s 的输出确认），所以 `/` 列表为空；
  输入框对空列表的提示改成"还没有列出命令：启动后才会列出，有的智能体不提供命令"，冒烟对没有命令的 ACP 智能体不再判失败。
  OpenCode 本机没装，没跑。
- **断网时的"未送达"**（`E2E_SCRIPT=tools/desktop-chat-offline-e2e.js python3 tools/desktop-chat-e2e.py`，debug 桌面包 +
  本机隔离被控端安静模式，3 次全过）：对话开始后停掉被控端，再发的消息 0.1 s 内显示"未送达：连接断开了"，带重新发送和丢弃；
  重启被控端，面板自己重连，点重新发送后送达，智能体只回答一次。e2e 脚本加了 `(action) stop-host` / `(action) start-host`
  两个动作，由 `desktop-chat-e2e.py` 执行。
  "被控端在线但 15 s 没有确认"这条路径仍只有单测（要让被控端收下命令却不回，测试里做不到不改代码）。

### 未测 / 没做

- ~~Windows 桌面端（WebView2）：Highlight API、粘贴图片、一行布局~~（2026-10-22 已测，见"Windows 补测"一节）；拖入仍未测。
- ACP 智能体的图片只有单测和替身（DeepSeek Harness 不收图片；收图片的 ACP 智能体没跑过）；OpenCode 没跑。
- Codex 启动后第一条消息若在 `skills/list` 回来之前发出，消息里的 `$技能` 不会带 skill 输入项（Codex 是否仅凭文字识别没验证）。
- 发送状态的"15 s 无 ack 判未送达"只有单测（断线判未送达已补测，见上）。
- 查找只搜已挂载的内容；引用只在鼠标 / Shift 键松开时出现，触控板长按选择没试。
- iPhone 端没有跟进（`feat/ios-chat`）。

## 去掉单独的"插话"操作，按智能体原生的方式发送（2026-10-22，分支 `claude/desktop-chat-e2e`）

用户："不需要有插话这个操作吧？就用原生 agent 的概念就行了。"Claude Code、Codex 自己就是这样：智能体工作时你照常发消息，
它在下一步把消息并进正在做的这一轮。所以界面不再给"插话 / 排队"两种选择：

- 输入框只有一个发送。运行中发送的方式由智能体决定（`sendMode`）：`caps.steer` 为真（Claude Code、Codex）发
  `mode: "steer"`，否则不带 mode（排队，本轮结束后发）。去掉芯片、`hc_mode_*` 文案和样式。占位文字分别是
  "它工作时也可以继续发消息" / "本轮结束后它会收到这条消息"。
- 并进当前轮的消息（`user` 带 `steer: true`）显示成普通用户消息，仍在这一轮里，不加标签（保留 `data-steer` 供测试用）。
- 排队的消息照旧显示在输入框上方，可编辑、撤回；"现在插进这一轮"按钮改叫"立即发给它"。`steer_queued` 提示改成中性的
  "智能体这一轮结束后会收到这条消息。"
- 协议没变（`caps.steer`、`input.mode`、`user.steer`、`steer_queued` 都还在）；iOS 端同样的改动交给 PR #22 的会话。
- 验证（2026-10-22，本机，debug 包）：`tsc` 过，前端测试 84 过；e2e 替身流程 3 次全过（排队 / 撤回改在不支持插话的
  智能体上测），真实 Claude Code 1 次 13 项全过：第 2 条命令后发的消息作为普通用户消息进了同一轮，Claude 数到 5 停下。

## 对话侧栏：选工作目录、按项目列会话（2026-10-22，分支 `claude/chat-folder-5ebe7f`，基于 `feat/host-chat` 0071902）

用户要求"选择工作目录"和"显示已选目录"照 Codex 桌面版做，并参考 Orca；第一版出来后用户嫌项目和会话列表丑，要求照人家的设计。

### 调研

- **Codex 桌面版**（本机没装，依据官方文档 developers.openai.com/codex/app，未实机）：新对话先选"项目"（一个本地文件夹），用过的项目都会出现在选择器里，没有的就"添加项目"选文件夹；侧栏按项目列线程。
- **Orca 1.4.237**（`/tmp/orca-app` 解包 + 之前的 CDP 截图 `/tmp/orca-cdp/r4.png`）：左栏"项目"按项目分组，组头是文件夹图标 + 项目名，组内每个工作区 / 会话一张卡（状态点 + 名字，下一行灰色元信息：智能体 · 多久前）；右栏会话历史按项目分组带数量。远端主机上的项目只有一个路径输入框（`/path/to/project/on/host`），**没有远端目录浏览器**。native chat 本身不管选目录。
- 结论：选择器 = Codex 的"项目 chip + 下拉（最近项目 + 浏览 / 添加）"，会话列表 = Orca 的"项目分组 + 会话卡"。远端浏览 Orca 没有，我们用被控端现成的 `files_list` 补上。

### 做了什么（只改桌面端，被控端没动）

- `ui/chat/FolderPicker.tsx`（新）：
  - `FolderPicker`：一个 chip（文件夹图标 + 项目名 + 灰色的上级路径，家目录缩成 `~`），点开是下拉：搜索框（搜最近的文件夹，或输入路径）、"最近"（本机记的 + 被控端上开着的会话的目录，去重）、"浏览这台电脑上的文件夹…"。浏览模式有面包屑、`..`、子文件夹（隐藏 `.xxx`，输入 `.` 开头才显示），底部"使用「名字」"。输入路径回车就浏览那个路径；列不出来（策略拒绝、不存在）时显示原因，并给"仍然使用这个路径"（`chat_start` 会再校验）。键盘：↑↓、回车（进入 / 选中）、⌘/Ctrl+回车（选当前浏览的文件夹）、空输入时退格回上级、Esc（先回最近列表，再关闭）。没有最近目录时一打开就浏览 `~`。
  - `FolderLabel`：会话头部、会话列表、chip 共用；悬停显示完整路径。
  - 浏览用 `files_list`（admin 级、默认放行、只读，和 `chat_start` 同为 admin，所以能启动智能体的设备就能浏览）。只取第一页（500 项，文件夹排在前面），文件夹超过一页时提示"输入名字缩小范围"。`files_list` 返回的 `path` 是展开后的，列 `~` 时顺便记下被控端的家目录（localStorage `ai-remote.chat.home`），之后显示路径时缩成 `~`。
- `lib/chat.ts`：路径纯函数 `looksLikePath / trimFolder / folderParts / folderName / parentFolder / childFolder / folderCrumbs / sameFolder / tildeFolder / folderChoices / matchFolders / listedFolders / sessionsByFolder / agoParts`，以及 `knownHome / rememberHome`。Windows（`D:\work`、`D:/work`、`\\server\share`、大小写不敏感）和 Unix、`~` 都覆盖。
- `AgentChat.tsx`：`StartForm` 的文本框 + datalist 换成 `FolderPicker`（新增可选 `folders` 参数，`Picker` 传开着的会话目录；`ComputerTabs` 不传，照旧可用）；`Picker` 的会话列表改为"项目"分组（组头：图标 + 名字 + 上级路径 + 数量；会话卡：状态点 + 智能体（· 模型）+ 多久前，下一行"就绪 / 运行中 / 已暂停 · N 轮 · 发起设备"）；`Conversation` 只改了头部那个显示 cwd 的 span。
- 文案：`hc_folder_*` 一段，加在 en.ts / zh.ts 末尾。样式：`native.css` 末尾两节。`hc_cwd_ph` 已不用，没删（避免和别的会话冲突）。

### 踩过的坑

1. **全局短类名会撞**：第一版高亮行用 `.act`，`v2-pages.css` 里 `.act` 是活动页的 `position:absolute; inset:0`，高亮行被拽到选择器顶上；"未选择"状态用 `.empty`，全局 `.empty` 是竖排居中的空态布局，图标和文字上下叠。改成 `.on` / `.none`。新组件里的类名先在 `styles/*.css` 里搜一下有没有顶层规则。
2. 路径的灰色部分用 `direction: rtl` 让开头省略，内容要包一层 `<bdi>`，否则 `/`、`~` 这类中性字符会被重排到另一头。

### 实测（2026-10-22，本机 macOS，debug 包，隔离被控端 `[network] quiet = true`）

- `node --test tools/*.test.mjs` 92 过 / 0 失败 / 2 跳过，其中新增 `tools/chat-folder.test.mjs` 8 项（路径解析、两种平台、`~` 缩写、去重、筛选、按项目分组、多久前）。`npx tsc --noEmit -p apps/desktop` 通过。
- `tools/desktop-chat-e2e.py`（替身智能体）**全部通过，共跑 3 次**（后两次是改样式后的重跑）。新增的检查：未选时 chip 提示选择；打开后经真实通道列出被控端家目录（46 个文件夹，隐藏的不显示）；输入路径出现"打开 …"、回车浏览、点子文件夹面包屑更新、底部"使用「work」"、chip 显示 `work` 且悬停是完整路径；会话头部显示项目名 + 上级路径；第二次启动默认选上次的文件夹、打开是最近列表且当前项打勾、会话列表按项目分组。截图 `/tmp/chat-folder-e2e/`（`00-folder-browse`、`04b-folder-recent`、`01-first-turn`）。
- `tools/desktop-chat-e2e-real.js`（真实 Claude Code 2.1.307，本机账号）：2026-10-22 用 main 8eeabb6 之后的 `claude/chat-folder-win` 跑了 1 次，**14 项全过**。选择器输入路径 → 回车 → "使用"，选到 `work`；会话头部显示 `work` 和上级路径；首轮 6 s 完成（建文件 + `cat`）；长任务中途发消息让它数到 5，最终 5 条命令。截图在 `/tmp/chat-folder-real/`。

### 未测 / 没做

- Windows 被控端：2026-10-22 用 Mac 上已装的命令行对 win-dev 的真实被控端（服务模式）只读调用 `ls`（即 `files_list`）核对过返回的路径：`~` → `C:\Users\<用户>`，`~\Desktop` → 重定向后的桌面（另一个用户目录下，选择器不会缩成 `~`），`C:\`、`D:/`、`C:\Users\`（带结尾分隔符原样返回）、`\\localhost\C$` 都能列；单独的 `C:` 会被拒，选择器发请求前已补成 `C:\`。这些返回值写进了 `tools/chat-folder.test.mjs`。顺带发现盘根下的 `$RECYCLE.BIN`、`$GetCurrent`、`System Volume Information` 没有隐藏标记会被列出来，`listedFolders` 现在按名字把它们藏起来（输入 `$` 或名字开头时才显示）。**桌面端选择器本身没在 Windows 被控端上点过**；被控端也列不出盘符清单，要换盘只能输入 `E:\`。
- `files_list` 被机主改成 ask / deny 时：ask 会每进一个目录弹一次审批，deny 时只能输入路径；只按代码推导，没实测。
- 演示模式（`lib/demo.ts`）没有 `files_list`，浏览会显示"无法列出"。
- 旧被控端（没有 `chat_*` 工具，例如 2026-10-22 win-dev 上装成服务的那份）原来在对话侧栏里显示原始错误 `Mcp error: -32602: unknown tool 'chat_runtimes'`。版本号都是 0.0.1，分不出新旧，所以按这个错误判断（`chatUnsupported`），侧栏和电脑页改成黄色提示"被控端版本较旧，更新后再试"。只过了单测和 tsc，没重编 app 看。
- 和发起会话的约定：本分支的 e2e 仍含插话步骤；去掉插话的分支合进来时要一并改 e2e。

## 对话里的文件预览（2026-10-22，分支 `claude/chat-preview-97901c`，基于 `feat/host-chat` 0071902）

需求：智能体创建或修改的 HTML、Markdown、图片，在桌面端对话侧栏里能直接预览。

### 调研（Orca 1.4.237 安装包，`app.asar` 解包后读代码；Codex / Claude 只有公开资料）

- Orca：HTML 不是任意文件都预览，而是智能体按约定写进对话专用临时目录，再在回答里写 `::orca-visual{file="x.html"}`，回答里内联一个 `<iframe srcDoc sandbox="allow-scripts">`，页面最前面插 CSP（网络全断，脚本只能来自几个 CDN），点开在编辑器标签页；图片是 80 px 缩略图，点开是可缩放弹层；Markdown 文件点开在编辑器标签页的 MarkdownPreview，对话里不内联。
- Codex 桌面版：官方文档只说 review 窗格看 Git 改动、点文件用外部编辑器打开；"产物预览"只见于第三方和社区，可信度低。Claude artifacts 是对话右侧固定面板。

### 做法

- **形态**（照 Orca，用户要求"会话内也要显示"）：每轮结束后，在轮尾内联显示这一轮写出 / 点名的可预览文件（`lib/preview.ts` 的 `turnFiles`：工具写的 diff 路径和非读非删工具的位置，再加最后一段回答里链接 / 行内代码点名的文件；按文件名去重，最多 4 个）：HTML 是 220 px 高的小窗，Markdown 是前 180 px（渐隐），图片是 96 px 高的缩略图。点卡片、工具行 / 编辑卡上的眼睛按钮、回答里的文件链接或 `out/x.md` 这样的行内代码，都打开全窗口弹层（560 px 侧栏放不下页面）：HTML / Markdown 可切"预览 / 源码"，图片点一下在适应窗口和实际大小之间切换，Esc 关闭。内联卡片接近屏幕时才取文件（IntersectionObserver，页面隐藏时直接取），同一会话里每个文件只取一次（`PreviewProvider` 里按路径缓存 Promise，失败不缓存，弹层的"重新获取"强制再取）。
- **被控端新工具 `chat_file`**（`crates/ar-host/src/tools/chat.rs`、`chat/preview.rs`）：参数 `session`、`path`；admin 级、默认允许、只读；返回一次性票据，字节走现有的 `app/ai-remote/file/1`（与 `files_pull` 同一个 `Transfers`，审计照常记 `transfer`，工具名 `chat_file`）。没有复用 `files_pull`，因为它默认要机主审批，每次预览弹一次审批不可用。限制：
  - 路径相对会话工作目录解析（`~` 是用户主目录），解析符号链接后**在工作目录内**，或者这个路径（原样或解析后）出现在本会话**智能体自己的事件**里（工具调用的 locations、diff 的 path、智能体回答文字；人发的消息不算）；
  - 解析后的真实文件扩展名是 html/htm/md/markdown/png/jpg/jpeg/gif/webp/svg/bmp；
  - 最大 16 MiB。
  - 为什么默认允许：调用的设备已是 admin、能和这个会话对话，本来就能让智能体（以用户身份、不询问）把任意文件打印出来；`chat_file` 只省掉这一来回，并且限制在对话涉及的文件上。机主仍可用 `[[rule]] tool = "chat_file"` 改成询问或拒绝。`chat_file` 加进了控制端的 `CHANNEL_TOOLS`，不对 MCP 客户端列出。
- **Tauri**：`preview_file(host, session, path)` → `transfer::preview`，在内存里收字节并核对 SHA-256，以原始字节（`Frame`，页面里是 `ArrayBuffer`）返回；不写控制端磁盘。
- **HTML 的安全取舍**：Orca 允许脚本（`allow-scripts`，无 same-origin）；我们**不运行任何脚本**：`sandbox=""`（无脚本、opaque origin、无表单 / 弹窗 / 顶层导航），文档 `<head>` 最前面插 `default-src 'none'` 的 CSP 加 `<base target="_blank">`（不发任何网络请求；链接点了是弹窗，被沙箱拒绝）。原因：WKWebView 里 Tauri IPC 背后的原生消息处理器对所有 frame 可见，没把握让智能体写的脚本跑在应用的 webview 里；而且应用自己的 CSP（`script-src` 回落到 `'self'`）会被 srcdoc 继承，内联脚本本来也跑不了。代价：依赖脚本的页面（图表库）只显示静态部分；页面引用的相对路径图片、外部字体 / CSS 不加载。Markdown 用对话自己的渲染器（只产出文本节点）；SVG 走 `<img>`，脚本不执行。

### 实测（本机 macOS，2026-10-22）

- 单测：`cargo test -p ar-host --lib chat` 37 项全过（新增 3 项：扩展名、"只认智能体的事件"、`previews_read_only_files_the_session_is_about` 用真实会话覆盖目录内 / 目录外未点名 / 符号链接逃出目录 / 人发消息不算 / 智能体写过后放行 / 超 16 MiB）；`node --test tools/*.test.mjs` 92 项 90 过 2 跳过（新增 `tools/chat-preview.test.mjs` 6 项）；`tsc --noEmit` 和 `cargo clippy --workspace --all-targets -D warnings` 通过；debug 构建重新生成的 `bindings.ts` 与手写一致。
- 端到端（`E2E_SCRIPT=tools/desktop-chat-preview-e2e.js python3 tools/desktop-chat-e2e.py`，debug 桌面包 + 本机隔离被控端，两端 `[network] quiet = true`；替身智能体的 FILES 轮在工作目录写 report.html / notes.md / chart.png、在目录外写一个它点名的 named.md、另放一个没点名的 secret.md）：**24 项全过**（最后一次；之前一次 18 项版本也全过）。覆盖：三种入口、4 个内联预览全部加载、缩略图 64×32、内联和弹层的 iframe 都是 `sandbox=""` 且 CSP 在页面 `<script>` 之前、页面脚本没运行（它会 `parent.postMessage`，1.5 s 内没收到）、外部图片没加载（截图里只显示 alt 文字）、源码视图、Esc、目录外点名的可读、没点名的被拒（"outside this session's folder"）、`.toml` 被拒（"cannot be previewed"）。截图在 `/tmp/chat-preview-e2e/`，没进仓库（推流画面里是本机桌面）。
- 演示模式（camofox，`npx vite` + 演示后端）：三种内联卡片和弹层的深色截图看过。
- 耗时：脚本记的"点击 → 弹层就绪"是 50~51 ms，但脚本按 50 ms 轮询、且内联已预取（弹层命中缓存），所以只能说明本机回环下 ≤ 约 50 ms；**未测**冷取时延和跨机器时延。

### 实测（Windows 开发机 win-dev，WebView2，2026-10-22，分支 `claude/win-preview-e2e`）

- 环境：Windows 11、WebView2 155，origin/main 8eeabb6 的 debug 构建（`cargo build -p ai-remote` + `npx tauri build --debug --no-bundle`，独立 worktree `%USERPROFILE%\ai-remote-preview`），被控端和控制端都是隔离目录、`[network] quiet = true`，经 zremote 在交互会话里跑；没碰真实被控端服务。
- `E2E_SCRIPT=tools/desktop-chat-preview-e2e.js python tools/desktop-chat-e2e.py`：脚本本身不用改就能在 Windows 上跑（win-verify 那次已经做好：PrintWindow 截图、`.exe`、WebView2 数据目录），只修了一处选目录的匹配（见下面的坑）。**原 25 项连跑 3 次全过；加上两项路径显示检查后 27 项连跑 2 次全过**，另 1 次 27 项输出完整、没核对结尾。即：内联四张卡片都加载、缩略图 64×32、内联和弹层 iframe 都是 `sandbox=""`、CSP（`default-src 'none'`）在页面 `<script>` 之前、app 拿不到 frame 的 document（opaque origin）、页面脚本没运行（1.5 s 内没有 `postMessage`、标题没变）、源码视图、Esc、Markdown 表格、PNG 尺寸、目录外点名的文件可读、没点名的和 `.toml` 被被控端拒绝。截图（没进仓库，推流画面里是 win-dev 桌面）看过：HTML 弹层里外部图片只显示 alt 文字，和 Mac 一致。
- `<bdi dir="ltr">` 路径（新加的两项，用 Range 量每个字形的位置）：Windows 路径 `C:\Users\…\outside\named.md` 在 `direction: rtl` 的框里，首字符 `C` 仍在最左、末字符在右边界内；把框临时收窄到 160 px 后，开头被截在框左外、`named.md` 仍完整可见。宽窗口下路径本来就放得下，所以截断只靠收窄那一项覆盖。
- 耗时：点击 → 弹层就绪 50~55 ms（同样是 50 ms 轮询 + 内联已预取，只能说明 ≤ 约 50 ms）。
- 新加的两项和 `.fn` 匹配的改动**没在 Mac 上重跑**（Mac 上没有现成的构建）。
- **未测**：跨机器（预览字节走真实网络）的冷取时延；HiDPI 缩放；真实智能体写出的文件。

### 踩过的坑

1. 被控端工具的 `json()` 输出多行美化 JSON，而 `transfer.rs` 只解析结果第一行，第一次端到端全部报 "EOF while parsing"；`chat_file` 改成和文件工具一样输出单行。
2. 弹层头部的路径用 `direction: rtl` 做左侧省略，开头的 `/` 会被挪到末尾；里面包一层 `<bdi dir="ltr">`。会话头部的工作目录（`AgentChat.tsx` 原有）也有同样的问题，没改。
3. demo 的 `demoLog()` 在模块加载时执行，用到的常量必须定义在它前面（否则 TDZ）。
4. Windows 上 `Icon` 是 Segoe Fluent Icons 字形（私用区字符，`chevR` 是 U+E76C），会进 `textContent`，`trim()` 去不掉：选目录时按整行文本找 `work` 一直找不到（行其实 1 s 内就出来了）。e2e 改为比较 `.fn` 的文本（预览和主 e2e 两个脚本）。中途误以为是 WebView2 定时器节流，加过后台节流开关，没用，已撤回。

### 没做 / 下一步

- [x] Windows（WebView2）上看一遍：`sandbox=""`、srcdoc 里的 CSP、`bdi` 都正常（见上面 Windows 实测）。
- [x] 跨机器真实智能体：见下一小节。
- [x] iPhone 端的预览：#32 已在 main。
- [x] "允许脚本"：见下一小节。"保存到下载"按钮（复用 `pull_file`，会走 `files_pull` 的审批）没做。
- [x] 会话头部工作目录的 rtl 斜杠错位：main 上目录选择那边已改成 `FolderLabel` + `<bdi>`，演示页实测正常（`<bdi>` 不写 dir 时按规范没有强字符就是 ltr，`/2026/10` 也对）。

### 服务模式下的读取身份、签票即读、运行脚本的独立窗口（2026-10-22，分支 `feat/chat-preview-scripts`，基于 main a7df1d3）

**发现的问题（读代码确认，没有真机复现）**：`chat_file` 在被控端进程里读文件，兑换票据时还会按路径重新打开。
- Windows 服务模式下被控端是 SYSTEM，智能体经 chat-runner 以用户身份运行。"默认允许"的理由（"智能体本来就能把文件打印出来"）在这里不成立：管理员设备让智能体在回答里**提到**任意路径（别的用户的图片、Markdown），`chat_file` 就以 SYSTEM 读出来，绕过了 `files_pull` 的审批。
- 签票时检查、兑换时重开：2 分钟内文件可以换成指向别处的链接（扩展名和范围检查被绕过；用户态下智能体本来也能读，但范围限制失效）。

**改法**：
- 签票时就读字节（`chat::preview::read_file`：重新解析后必须仍是检查过的那个路径、扩展名、≤16 MiB），票据带着字节（`Direction::Bytes`，所有票据合计最多 128 MiB，超出丢最旧的），文件通道直接发这份字节（`deliver_bytes`）。
- 读的身份：本地后端由被控端进程自己读；runner 后端（SYSTEM 被控端）走 runner 新加的 `read` 请求，由用户会话里的 runner 以用户令牌读，base64 过管道（单行上限 40 MiB，16 MiB 编码后约 21 MiB）。

**运行脚本**（`apps/desktop/src-tauri/src/preview_window.rs`，Tauri 命令 `open_preview_window`）：
- 读 Tauri 2.12 源码（`webview/mod.rs` 的 `is_local_url` / `on_message`）的结论：**应用自己注册的自定义协议算"本地来源"，本地来源在没有应用 ACL 清单时可以调全部应用命令**，所以不能用自定义协议开这个窗口。非本地来源要过 ACL，capability 只给了 `main`，所以应用命令和插件命令都被拒；Channel 数据按 webview 标签分开存（`ipc/channel.rs`），另一个窗口读不到主窗口的。
- 做法：应用内一个只听 `127.0.0.1`、端口随机的极简 HTTP 服务，每个页面一个随机路径，别的路径 404；响应头带 CSP（内联脚本 + jsDelivr / unpkg / cdnjs / esm.sh / Google Fonts 的脚本和样式，`connect-src` / `frame-src` / `form-action` / `base-uri` 全 `none`，图片只许同源、`data:`、`blob:`）、`no-store`、`nosniff`、`no-referrer`。窗口 `incognito`、只能停在自己的页面（`on_navigation`）、不许开新窗口（`on_new_window` → Deny），关窗即丢弃页面。debug 的 smoke 模式下窗口放到屏幕外。
- `preview_server_requests` 命令返回最近 64 个被请求的路径（只有路径），给 e2e 判断页面做了什么。

**实测（本机 macOS，debug，2026-10-22）**：
- `cargo test -p ar-host --lib chat` 62 过（新增 `a_preview_is_read_by_the_runner`、`a_file_swapped_for_a_link_after_the_check_is_not_read`）；第一次整跑时 `an_image_crosses_the_pipe_to_an_agent_in_the_runner` 超时 1 次，单独跑和随后整跑都过（机器繁忙，未再现）。`cargo clippy --workspace --all-targets -D warnings` 无警告；`node --test tools/*.test.mjs` 115 过；`tsc` 过；debug 构建重新生成的 `bindings.ts` 与手写一致。
- 预览 e2e（替身智能体、本机隔离被控端，安静模式）：连续 4 次全过（每次 32 项）。"运行脚本"的三项：页面在独立窗口里跑了脚本（服务收到 `ran`）；页面里 `window.__TAURI_INTERNALS__` 存在但 `invoke` 不是函数（`ipc-threw`），调不到应用；`fetch("https://example.com/")` 被拦（`fetch-blocked`）。此前 2 次运行在第 137 行报"找不到源码按钮"：截图里那时弹层已经不在了；加了诊断（弹层消失时记录面板是否被换、最近的按键）后 4 次都没再出现，每次弹层消失前都是脚本自己的 Esc。**原因没查清**，诊断留在脚本里。
- #30 加的"长路径从左侧截断"检查在 WebKit 下失败，截图显示截断是对的（`…c4mmn/outside/named.md`）：WebKit 把被省略的第一个字符位置报在省略号之后，检查改为"内容比框宽 + 框是 rtl + 最后一个字符完整在框内"。
- **跨机器、真实智能体**（`tools/desktop-chat-preview-remote.py` + `tools/desktop-chat-preview-real.js`；win-dev 上本分支 debug 版的**用户态**测试被控端、隔离目录、安静模式；Mac 上 debug 桌面包、隔离控制端目录、安静模式；Claude Code 2.1.293，1 次）：一轮 19.4 s 写了 report.html / notes.md / chart.svg，三张内联卡片全部加载（截图里中文页面、表格、SVG 都对）；冷取（每次新票据 + 字节过 QUIC）：642 B 32~41 ms、147 B 31~44 ms、715 B 37~43 ms、4.2 MB PNG 275~349 ms（约 12~15 MB/s），各 5 次；"运行脚本"页面被独立窗口取走。线路是直连还是中继没记录。

**未测**：
- **服务模式（runner）的真机读取**：要把 win-dev 服务 `ai-remote-host-cc166525` 换成本分支的 release 构建，等于重启真实被控端，需要用户点头；本分支只有替身 runner 的单测。步骤：win-dev 上 `cargo build --release -p ai-remote` → 停服务 → 把 `ai-remote-hostw.exe` 等复制到 `$HOME/ai-remote-svc-bin`（服务的 binPath）→ 起服务 → Mac 上用桌面包对 win-dev 开一个会话，让智能体写一个 html，再让它在回答里提到一个只有管理员能读的文件（如 `C:\Windows\System32\config\systemprofile` 下的某个 `.md`）→ 预期前者能看、后者报"cannot read"。
- 真实窗口里"运行脚本"窗口的观感（e2e 里窗口在屏幕外）；Windows（WebView2）下的独立窗口；依赖 CDN 的真实页面（例如 Chart.js）能否画出来。
- 弹层偶发自己消失的原因（见上）。

## 桌面端对话侧栏端到端（2026-10-22，分支 `claude/desktop-chat-e2e`，基于 `feat/host-chat` 4be9e12）

`feat/host-chat` 合入桌面新界面、被控端插话之后的第一次端到端：真实的 debug 桌面应用，经 Tauri → 控制端代理 → QUIC →
本机隔离被控端（安静模式）→ 智能体进程。

- 驱动方式：debug 版的 smoke 模式加了 `AI_REMOTE_SMOKE_SCRIPT=<file.js>`，页面加载后执行脚本，脚本直接操作真实 DOM
  （React 受控输入框用原生 setter + `input` 事件）；页面用 `log_from_page('shot', name)` 要截图（WKWebView snapshot，
  窗口在屏幕外也能截），`log_from_page('done', …)` 结束。只在 `debug_assertions` 下编译。
- `tools/desktop-chat-e2e.py`：起隔离被控端（两个替身智能体：`Rich fake` 走 Claude stream-json，RICH 时发思考、TodoWrite
  计划、Read、失败的 Bash、Grep、Edit、带标题 / 列表 / 表格 / 代码块的回答、`modelUsage` 上下文；SLOW 时一直运行到下一条
  消息〔插话〕或停止；`No-steer fake` 是同一个程序加 `steer = false`），桌面 profile 用 `AI_REMOTE_HOME`，所有 home 的
  host / controller 都写 `[network] quiet = true` 并断言 doctor 的 network 是安静。`tools/desktop-chat-e2e.js` 是替身
  流程，`tools/desktop-chat-e2e-real.js`（`--real "Claude Code"`）是真实智能体：建个文件 + 一个按顺序跑 20 条命令的任务，
  跑到第 2 条时插话"改成数到 5"。

### 实测（2026-10-22，本机 macOS，debug 包）

- 替身流程 **36 项全过**（连续 2 次）：启动表单列出智能体并启动、首条消息、思考折叠、计划 3/3、工具折成一句
  （"读了 1 个文件，运行了 1 条命令，搜索了 1 次 和 编辑了 1 个文件 +2−1 · 1 个失败"）、展开后失败命令红底输出、diff
  +2 −1、Markdown 标题 / 列表 / 表格 / 代码块、上下文环 42k / 200k；运行中默认插话、插话进同一轮带标记、不新增轮；切排队、
  排队行、撤回、停止后撤回的消息没跑、"继续"发出新一轮；不支持插话的智能体只有静态"排队"芯片，消息排队，停止后作为下一轮
  跑；会话列表两条、回到第一个会话回放 4 轮。截图 `/tmp/desktop-chat-e2e/`。
- 真实 Claude Code 2.1.307（本机账号）**13 项全过**（1 次）：首轮 6 s 完成，命令显示为工具行，有回答，上下文环
  "129k / 1.0M"；长任务按顺序跑命令，被折成"运行了 N 条命令"；第 2 条命令后插话，2 s 内以"插话"标记进同一轮，最终共
  5 条命令（数到 5 停下）。同一天早些时候 Claude 报 401（OAuth 令牌过期，`claude auth status` 仍说已登录），面板正确显示
  重试提示和"失败: Failed to authenticate…"。
- Codex 0.137.0：账号对默认模型报 400（"The 'gpt-5.5' model is not supported when using Codex with a ChatGPT account"），
  面板正确显示"失败: …"，真实任务没跑成。提问卡片（只有 Codex 适配会发 `question`）因此**没在真实连接上测**。

### 发现

1. **（已修，b077920）** 面板按主机名在 localStorage 记住上次的会话（`ai-remote.chat.last`）；该会话在被控端已不存在时，
   原来只显示"no chat session '…' on this host"和一个发不出去的输入框。b077920（`sessionGone()`、提示 `hc_session_gone`）
   之后回到会话列表并提示。e2e 第 0 步复现它：在 localStorage 写一个不存在的会话再切回对话标签。修之前的包这一步失败；
   在 `feat/host-chat` 0071902 上 5 次都过。
2. **（已修，b077920）** 面板原来只用 `requestAnimationFrame` 合并事件，窗口不可见（别的 Space、最小化、屏幕外）时
   rAF 不触发，事件积在内存里不显示；运行计时显示"0 ms"。b077920 加了 100 ms 定时器兜底、计时从轮次开始取当前时间。
   脚本里原来的 rAF 替换已去掉；0071902 上 5 次运行里 4 次全过，运行中计时 2.5 s 后显示"2.1 s"。
3. debug 包与安装版同一个 bundle id（`io.github.jia-ss.ai-remote`），共用 WebKit 存储；脚本每次用新的主机名
   `e2e-<pid>`，结束时删掉自己写的 `ai-remote.chat.*` 条目。
4. 真实 Claude 一轮要带约 129k tokens 的上下文（本机全局 Claude 配置），慢且贵；按顺序的命令会被折叠，检查命令数要看 run
   的句子，不能搜 `echo N`。

5. **（误报，已核实）"对话面板会被整体重新挂载"**：原来的检测只看 `.nt-chat` 元素有没有换，把脚本自己的动作（切"步骤"再切回
   "对话"、回会话列表、换会话、重新打开会话）都算了进去，每次 2~4 次正好是这几步。桌面对话会话加挂载日志跑了 3 次核实：
   SessionPage 从未卸载。检测已改成只在同一会话（目录 + 第一条用户消息相同）的面板被换掉时报。之前一次"计时、插话芯片"
   失败的原因没查清，插话芯片现在已去掉，之后没再出现。

### 没做

- [ ] 真实 Codex（账号问题）、提问卡片在真实连接上、Windows 开发机上的桌面端、真实窗口可见时的手感（滚动跟随、键盘）。

## Windows 上的对话：WebView2 界面与后台 hostw（2026-10-22，分支 `claude/win-verify-c5c3c1`，基于 `feat/host-chat` 54625ae）

补 Windows 上还没验证的两块：桌面端对话面板在 WebView2 里的渲染，以及用户平时用的后台被控端（计划任务里的 `ai-remote-hostw.exe`，GUI 子系统、没有控制台）拉起智能体。测试机 win-dev（Windows 11，5120×2160，WebView2 155，debug 构建，独立 worktree `%USERPROFILE%\ai-remote-winverify`）；所有测试被控端 / 控制端都是隔离目录、安静网络模式；没有碰真实被控端。GUI 相关的都经 zremote 在交互会话里跑（SSH 会话没有桌面）。

### 桌面端（WebView2）

- **`tools/desktop-chat-e2e.py` 现在能在 Windows 上跑**：可执行文件加 `.exe`，临时目录不再写死 `/tmp`，子进程输出按 UTF-8 读，给 WebView2 传 `--disable-features=CalculateNativeWinOcclusion`（窗口在屏幕外也继续绘制）和一个一次性的数据目录。截图走 `lib.rs` 里已有的 Windows `PrintWindow` 实现。
- **实测**：替身流程 35 项 ×3 次全过（Mac 的 36 项里有 1 项是"同一会话面板被重挂载"的诊断，Windows 上没触发，不计入）；真实 Claude Code 2.1.293 13 项 ×1 次全过：首轮 30 s，插话 3 s 内作为普通用户消息进同一轮，最终跑 5 条命令停下，上下文环 132k / 1.0M。截图在 Mac 的 `/tmp/win-chat-ui/`、`/tmp/win-chat-real/`、`/tmp/win-chat-render/`（没进仓库）。
- **新增 `tools/desktop-chat-e2e-render.js`**（`E2E_SCRIPT=tools/desktop-chat-e2e-render.js python tools/desktop-chat-e2e.py`）：查主流程没覆盖的渲染项。WebView2 上 10 项 ×2 次全过：
  - 长消息（40 行）折叠，`mask-image` 生效（计算值 `linear-gradient(rgb(0,0,0) 65%, transparent)`），可见高度 202 px / 全文 806 px，截图里末行渐隐；
  - 输入法组字时（`isComposing`、keyCode 229）回车不发送，普通回车发送。这是合成事件；真实输入法见下面"真实中文输入法"；
  - 6 轮时前 4 轮 `content-visibility: auto`、后 2 轮 `visible`，滚到顶后第 1 轮正常绘制（高 574 px）；
  - 发送后列表停在底部（距底 < 40 px）；
  - 字体：正文是 `Segoe UI Variable Text` → `Microsoft YaHei UI`，代码是打包进来的 Geist Mono。
- **看截图对比 Mac 的结论**：Markdown（标题、列表、表格、代码块）、代码高亮、diff 卡片（红绿底、+/− 列）、失败命令的红框、计划、运行组的句子、插话、停止后继续、排队卡片、回放，都和 Mac 一致；窗口更宽时 diff 行不换行。
- **修了两处**（`styles/native.css`）：
  1. 对话区滚动条在 WebView2 上是带上下箭头按钮的灰色经典滚动条。原因：`.nt-chat-body` 设了 `scrollbar-width: thin`，Chromium 一旦看到标准属性就不再用全局的 `::-webkit-scrollbar` 样式。加 `[data-os="windows"] .nt-chat-body { scrollbar-color: var(--stroke-2) transparent; }` 后是透明轨道 + 灰色细滑块（只剩很淡的小箭头）。只对 Windows 生效，Mac 不变。
  2. 等宽字体栈 `--font-data` 只有 `PingFang SC` 一个中文字体，Windows 上代码块里的中文会落到 `monospace`（宋体系）。在它后面加了 `"Microsoft YaHei UI"`。
- **坑**：Python 3.12 在 Windows 上 `tempfile.mkdtemp` 给目录设只限本人的 ACL，WebView2 的沙箱进程用不了里面的数据目录，启动时报 `0x80080005`（"服务器运行失败"）并在桌面上留一个"无法创建数据目录"的模态框——第一次跑时 app 就卡在这个框上 60 s、什么都不输出。数据目录现在放在临时目录外层（`%TEMP%\adc-webview-<pid>`），结束删掉。

### 后台被控端（hostw）

- **做法**：`tools/chat-smoke.py --hostw target/debug/ai-remote-hostw.exe`：隔离被控端用 hostw 起（监护进程 + 被控端子进程，日志写 `<home>/host/host.log`），其余同原来的冒烟。Windows 上它还会：打断真实智能体后跟踪那条 `sleep 45` 命令多久离开被控端的进程树（2026-10-22 起是判定项：10 s 内必须离开，见下面「Codex 打断后收掉本轮命令」）；`chat end` 后检查被控端下面不剩任何进程（判定）。
- **窗口监视** `tools/win-window-watch.ps1`：在交互桌面上用 WinEvent 钩子（`EVENT_OBJECT_SHOW`）记下运行期间出现的**每一个**可见顶层窗口（只闪几毫秒的控制台也抓得到），窗口出现的同时记下属主进程的父进程链；另外每 150 ms 轮询一次被控端下面的进程树，结束 3 s 后报告还活着的。
- **实测**（真实 Codex 0.162.1 `codex.cmd`、Claude Code 2.1.293 `claude.cmd`，都是 npm 垫片；每次 36 项：替身 27 + 真实智能体 9）：Codex 3 次、Claude 4 次都全过（Codex 第 1 次有 1 项失败：我当时把"打断后 3 s 命令还在"写成了判定项，之后改成只报告）。
  - **控制台窗口**：被控端树里的进程 7 次都**没有**弹过窗口。树里每次 110~200 个进程（conhost 40~70 个——都是隐藏的；node、cmd、git、bash/wsl、MCP 服务、`--version` 探测），全部没有窗口。监视器一共记到 12 个控制台窗口，都不属于被控端：
    - 1 个 Windows PowerShell 是我的测试脚本自己弹的（无控制台的 Python 调 `powershell` 查进程树），加 `CREATE_NO_WINDOW` 后再没出现；
    - 1 个 PowerShell 的父进程链是 `svchost(2832) <- services.exe`，是机器上的计划任务，和测试无关；
    - Claude 第 1 次运行中 1.2 s 内 9 个 `git.exe` 窗口 + 1 个 PowerShell：当时监视器还没记父进程链，归属不明；之后 3 次 Claude 都是 0 个。**这一条没有完全排除**，如果有人在桌面上看到 Claude 会话时闪 git 窗口，先用 `win-window-watch.ps1` 抓父进程链。
  - **打断**：Claude 被打断后 0.7 s（3/3）那条命令就离开了进程树；**Codex 打断后不杀正在跑的命令**，`sleep 45`（它选的是 WSL 的 `bash.exe`）跑满 45.3~46.6 s 才自己退出（2/2 次有计时；第 1 次只看到 3 s 后还在）。轮次本身 0.1 s 就结束了，下一轮正常。这是 Codex 的行为，被控端当时只在会话结束时收整棵树；已由适配器补上，见下一小节。
  - **结束会话**：`chat end` 后 2 s，被控端下面 0 个进程（7/7 真实智能体 + 7/7 替身）；整个测试结束后监视器报告的残留 0 个（7/7）。
  - 其余数字：一次任务 Codex 5.8~17.2 s、Claude 5.4~5.6 s；打断 0.1~0.2 s 生效；插话 Codex 0.08~0.25 s、Claude 3.96~4.40 s 收到 `steer:true`，Codex 最终 6 条命令、Claude 5 条。
- **推导**：智能体根进程带 `CREATE_NO_WINDOW`（`chat/proc.rs`），它和它拉起的控制台程序共用一个隐藏控制台；所以只有显式要求新控制台（`CREATE_NEW_CONSOLE`）或用 `DETACHED_PROCESS` 再起控制台程序的子进程才会弹窗。被控端本身有没有控制台对此没有影响。

### 未测 / 下一步

- 高 DPI 缩放、浅色主题的 Windows 截图。

### 真实中文输入法（2026-10-22，分支 `claude/ime-e2e`，基于 main 9933900）

- **做法**：`E2E_SCRIPT=tools/desktop-chat-e2e-ime.js python tools/desktop-chat-e2e.py --native-input`（2026-10-26 前是 `--visible`）。页面脚本发
  `(action) input {...}` 行，脚本用 `tools/win_input.py`（ctypes `SendInput`）在真实桌面上点输入框、把焦点窗口的键盘布局切到
  0804、逐键敲 `nihao`、空格、回车，按键和人打字一样经过微软拼音。`--visible` 让调试版窗口显示并可聚焦（`lib.rs` 的 smoke 模式
  原来一律 `set_focusable(false)`，现在只在屏幕外时这样）。它会动那台电脑的鼠标、改焦点窗口的键盘布局（结束时切回 0409），
  开着的大写锁定会先关掉、结束时恢复。
- **实测**（win-dev，Windows 11，微软拼音，WebView2，debug 构建）：3 次全过，每次 9 项：
  - 输入框拿到键盘焦点，敲 `nihao` 开始组字（输入框里是带下划线的 `ni'hao`，下面是"1 你好 2 … 3 泥嚎"候选框，整屏截图看到）；
  - 组字中按回车：输入法把字母 `nihao` 落进输入框，**没有发送**（用户消息数不变）；
  - 再敲 `nihao` + 空格：输入框是"你好"；回车：发出"你好"，输入框清空，替身智能体回"you said: 你好"。
  - WebView2 里被输入法吃掉的那次回车**根本不发 keydown 给页面**（3/3 次 0 个），所以 `Composer.tsx` 的 `isComposing` /
    keyCode 229 判断在这条路径上没被用到；Chrome 等会发 229，那条路径由合成事件测试覆盖。
- **踩过的坑**（都只是测试驱动的问题，界面本身没改）：
  - 驱动脚本所在的 Python 进程没声明 DPI 感知时，`ClientToScreen` 给的是虚拟化坐标，点击落到桌面上；改为 Per-Monitor V2。
  - 键盘布局按线程算：`WM_INPUTLANGCHANGEREQUEST` 要发给真正有焦点的窗口（WebView2 进程里的 `Chrome_WidgetWin_1`），发给应用的
    顶层窗口不管用。
  - win-dev 的大写锁定开着时，微软拼音直接出大写英文、不组字。
  - 新输入上下文里第一次组字要 1 s 以上才开始，固定等 1.2 s 不够，改成最多等 5 s。
  - 文件夹选择器要等进入目标文件夹（路径栏 `.cur` 变成它）再点"使用"，否则选不上，后面的"启动"会点到"使用"按钮。
- 服务模式（SYSTEM 被控端 + chat-runner）下的窗口与进程树：那是另一个分支 `feat/host-chat-service` 的事。
- ~~被控端在 Codex 打断后是否该替它收掉还在跑的命令~~：已做，见下一小节。

### Codex 打断后收掉本轮命令（2026-10-22，分支 `claude/codex-interrupt-kill`）

- **原因（读上游源码，openai/codex main 2026-10-22；不是推测）**：Codex 把 shell 命令跑成"后台终端"（unified exec，
  `core/src/unified_exec`）。`turn/interrupt` 走 `abort_all_tasks`，只取消这一轮的任务；后台终端只在
  `thread/backgroundTerminals/clean`、`.../terminate` 或会话关闭（`terminate_all_processes`）时才收。所以**这是 Codex 的
  设计，和平台无关**：Mac 上同样会留着（按源码推导，没在 Mac 上实测）。被控端的 Job Object 不影响它：命令本来就在被控端的树里，
  只是没人去杀。另外 Codex 在 Windows 上的进程组清理函数（`utils/pty/src/process_group.rs`）是空实现，Unix 上才真正杀进程组。
- **做法**（`chat/adapters/codex.rs`）：`initialize` 带 `capabilities.experimentalApi: true`（这两个方法是实验接口，不声明会被拒）；
  适配器记下本轮 `item/started` 里每个 `commandExecution` 的 id；本轮以 `interrupted` 结束时调
  `thread/backgroundTerminals/list`，对 `itemId` 属于本轮的逐个 `thread/backgroundTerminals/terminate`。
  终止由 Codex 自己做（Windows 上它给每个后台终端开了 Job Object），被控端不碰进程树。
- **取舍**：没有按"进程树里创建时间晚于命令开始"去收——Codex 自己知道哪个进程属于哪条命令，按 `itemId` 匹配不会误杀它的 MCP 服务，
  也跨平台。只收被打断那一轮开始的命令；之前几轮留下的后台终端（比如用户让它起的开发服务器）不动。被打断那一轮里起的长期进程也会被收，
  这和"停止这一轮"的意思一致。代价是 `experimentalApi` 打开后 Codex 会多发一些实验字段（适配器忽略未知字段）。
- **版本**：`list` / `terminate` 在 0.150（ChatGPT.app 内置）、0.162.1（win-dev）都有，0.137 只有 `clean`。没有这两个方法时
  `list` 报错，适配器只记一条 debug 日志，行为同修改前。没有用 `clean` 兜底：它会连前几轮的后台终端一起收。
- **测试**：单测 `an_interrupted_turn_stops_only_its_own_background_commands`（前一轮的不收、本轮的收、旧版报错时什么都不做）。
  `tools/chat-smoke.py --hostw` 的"打断后命令 10 s 内离开进程树"改成判定项。
- **实测**（win-dev，Windows 11，debug，后台 hostw，真实 Codex 0.162.1 `codex.cmd`，共 2 次；第 2 次全部通过：打断后命令 0.9 s 离开进程树，
  插话 0.08 s 进同一轮、Codex 跑 5 条命令答 `5`。下面是第 1 次；`tools/chat-smoke.py --quiet --hostw ... --real codex=...`）：
  打断 0.1 s 生效，被控端日志 `stopping a command of the interrupted turn command="sleep 45 && echo finished"`，那条命令
  **打断后 1.3 s 离开进程树**（修改前 45.3~46.6 s）；下一轮记得上文、看图、技能、`chat end` 后 0 残留都通过。
  这次唯一失败的是插话那项："the steer changes what it does"：插话 0.09 s 就进了同一轮，但 Codex 跑满 11 条命令、答 `11`
  （之前 3 次是 6 条）。改动只在轮次以 interrupted 结束后才发请求，插话这一轮没有被打断，所以判断与本改动无关，是模型没照插话改，
  第 2 次同样代码插话照常生效，所以按模型偶发记录（`experimentalApi` 是否影响插话没单独对照，2 次里 1 次正常）。Mac 上没跑。

## iPhone 上的编程智能体对话（2026-10-21，分支 `feat/ios-chat`，基于 `feat/host-chat`）

范围：会话列表、打开会话按轮显示、输入框和停止按钮、最小"新对话"表单（选智能体 + 文件夹 + 可选第一条消息）；
**不做** diff / 工具输出渲染。

- `ar-mobile`：`watch_chat(host, ticket, since, listener)` 走 `EmbeddedController::open_stream({"op":"chat"})`，
  回复（hello 行）放在 `MobileView.reply_json`，之后每个事件一行经 `MobileStream::event`；`chat_send(subscription, json)`
  把一个 JSON 对象重新编码成一行（保证单行），经有界队列按顺序写出，订阅已关时报错而不是静默丢弃。`request` 对
  `chat_start` 放宽到 330 s（要等机主在电脑上同意，`approval_timeout_secs` 默认 60），其他调用仍是 30 s。
- Swift：`PhoneChatModel.swift`（纯折叠，对应桌面端 `reduceChat`：用户行、正文 delta 合并、工具行只留状态和标题、
  思考折叠并显示秒数、`turn_end` 的结束原因、`seq` 去重；另有 `PhoneChatRuntime`）、`PhoneChatStore.swift`
  （`chat_sessions` → `chat_attach` 取票据 → 开通道；事件经 `AsyncStream` 串行进 MainActor，不用一事件一个 Task，否则
  delta 可能乱序；断开后 `resume()` 用 `since = log.seq` 续上；`start()` 调 `chat_start`，用返回的票据直接开通道，
  票据用不了再 `chat_attach`；被拒时把被控端原话显示出来）、`PhoneChatView.swift`（会话页工具栏"智能体"按钮弹出 sheet，
  列表右上角 + 打开新对话表单，启动中显示"如果电脑要求确认，请在电脑上允许"）。
- 输入法：iOS 上输入框是 `UITextView`，`markedTextRange != nil`（组字中）时发送按钮禁用、回车交给输入法；
  不在组字时回车即发送。
- 读写权限：`feat/host-chat` 的 7647923 起，任何管理员设备 `chat_attach` 都拿读写票据。旧被控端只给启动会话的设备
  读写，手机端保留"由 X 启动，这台 iPhone 只能查看"的分支，停止按钮在只读时改走 `chat_interrupt` 工具。

- **2026-10-21 晚 跟进桌面端新版对话界面**（桌面分支 `claude/desktop-chat-ui-f97e8c` 76139b2，照 Orca native chat 重做）：
  - 转换层照搬成 Swift（`PhoneChatModel.swift`）：`reduceChat`（steer 用户消息放进正在跑的轮、`usage.context_window/context_used`、
    被停止的工具记 `stopped`）、`phoneTurnRows`（user / text / run / edit / agent / thought / plan / question / notice）、
    `phoneRunSummary` / `phoneRunOutcome` / `phoneRunSentence`（文案同桌面 `hc_run_*` / `hc_live_*`）、`phoneDiffRows`（LCS 逐行 diff、
    3 行上下文、折叠 gap）、`stoppedTurn`、`phoneSendMode` / `phoneInputCommand`（queue 时不带 mode）/ `phoneEditQueued`。
    改桌面 `lib/chat.ts` 时同步改这里；`tools/PhoneChatModelTests.swift` 用的是桌面 `tools/chat-state.test.mjs` 的同一批用例。
  - 界面（`PhoneChatRows.swift` 对应桌面 `ui/chat/Rows.tsx`，`PhoneChatView.swift` 是对话流和输入区）：工具运行组一句话摘要
    （可展开到每条工具，命令 + 输出末 14 行）、diff 卡片（+N −M，展开逐行着色、gap 行）、思考折叠（"思考了 4.2 秒"）、计划卡、
    提问卡（编号选项 + 自由回答，回答后显示"已回答：x"）、"你停止了这一轮 [继续]"、上下文用量环、
    排队消息卡片（编辑=dequeue+重新排队、移出）、长按发送按钮"停止本轮并立即发送"（`mode: interrupt`）。
  - **2026-10-22 去掉单独的插话操作**（用户要求，照 Claude / Codex 原生 App 的做法）：只有一个发送按钮，没有插话 / 排队 chip，
    也没有"插话"标记和排队卡片上的"现在插进这一轮"。运行中发送时 `hello.caps.steer` 为 true 就带 `mode: "steer"`，否则排队
    （不带 mode）；智能体收下的插话（`user` 事件 `steer: true`）在当前轮里显示成普通的用户消息。不支持插话时消息照旧显示在
    输入框上方，可编辑、可移出；`steer_queued` 提示的文案改为"智能体这一轮结束后会收到这条消息。"。停止按钮保留。
  - Markdown 只渲染行内（`AttributedString(markdown:)`，不渲染 HTML），``` 围起来的代码块单独等宽显示。
  - 同步到桌面 8d7900c（Orca 实机对照后）：编辑并进运行组（行里没有 edit 了，组展开后编辑是 diff 卡片，组标题后加总的
    +N −M，`phoneRunDiffStat`）；一句话最后一项用" 和 "连接、编辑类文案"编辑了 N 个文件"；diff 开头 / 结尾不再放 gap；命令行只显示
    命令本身；失败数灰色"· N 个失败"；diff 行自动换行、复制按钮常显。
  - 没搬：↑/↓ 浏览发送历史、最近文件夹记忆（桌面端键盘 / localStorage 功能）、代码高亮；运行组箭头"悬停才显示"（手机没有悬停）。

### 验证

- `python3 tools/phone-chat-smoke.py`（只在 Mac）：`ios.sh mac-core` 把 `apps/ios/Sources` 全部编成 macOS 动态库，
  `tools/PhoneChatSmoke.swift` 用和 App 相同的 `PhoneClient` / `PhoneChatStore` 连隔离被控端（`chat-smoke.py` 里的替身
  智能体）：配对、`chat_runtimes` 与表单默认选中、不存在的文件夹被拒并显示原话、表单启动并直接开通道、列会话、
  命令行启动的会话也可写并回放、delta 合并、发送、忙时排队、停止得到 interrupted、排队消息接着跑、断开后续传不重复。
  2026-10-21 本机 22 项全过（第一版无表单时 18 项）。
- iOS CI：第一版曾推到 origin 跑过一次 workflow_dispatch 并通过；之后用户要求 `feat/host-chat` 相关改动先不进公开仓库，
  远端分支已删，**不要再推这个分支跑 CI**，iOS 编译和模拟器验证改在本机（xcodebuild / Claude 的 iOS 模拟器工具）。
  带新对话表单的这一版没有过 iOS 编译。
- 模拟器（2026-10-21，本机 Xcode 27.1，iOS 27.1 模拟器"iPhone Duo"）：`-ui-chat-fixture` 启动参数（仅 DEBUG）用演示事件
  打开对话视图，不连被控端（当时有"别起被控端"的要求）。手动在实时面板里点过：思考折叠展开；中文拼音键盘组字时
  （字母带下划线、有候选条）发送按钮是灰的、点了不发，回车把组字确认进输入框而不是发送，确认后发送按钮变蓝；发送后出现
  用户行、运行中的工具、"工作中"和红色停止按钮；停止后工具变失败、显示"已停止 · 1.2 秒"、回到空闲；返回看到会话列表；
  + 打开新对话表单（默认选中可用的智能体，未登录 / 未安装有标注，未安装的不能选），启动被拒时弹出原话。
  `apps/ios/Tests/PhoneChatUITests.swift` 自动走一遍发送、停止、回车发送、列表和表单，本机通过（21 s）；同一次 `xcodebuild test`
  第一次拉起 runner 时崩了一次（设备在 24A94425 运行时、SDK 是 24A94232），所以整体报 TEST FAILED，但用例本身 passed。
- 模拟器连真实被控端（2026-10-21，同一台 Mac；本分支 cherry-pick 了安静模式 55e2725）：隔离被控端和命令行控制端的
  `host/`、`controller/` 下都写了 `[network] quiet = true`（`doctor` 的 network 检查确认安静模式），模拟器里 App 的配置目录
  （`simctl get_app_container … data` 下的 `Library/Application Support/ai-remote`）首次启动前也放了同样两份。在实时面板里：
  粘贴票据配对 → 会话页"智能体" → 列出命令行启动的会话并回放 → 发消息得到回答 → 命令行发的慢任务实时出现在手机上、
  手机点停止得到"已停止 · 4.4 秒" → 新对话表单列出被控端真实检测结果（Claude Code、Codex 可用，其余未安装），选替身智能体
  启动后直接打开会话，命令行往这个会话发的消息实时显示。测试完被控端、控制端、模拟器 App 都已停掉。
- 没测：真机；连真实被控端时从手机输入大写英文（拼音键盘会把字母变成候选，这是键盘行为，改用命令行发的慢任务测的停止）。

- 2026-10-21 晚（新版界面）：`tools/test-phone-chat-model.sh`（Swift 6、warnings as errors，macOS 上跑）17 项全过；
  `python3 tools/phone-chat-smoke.py`（安静模式）24 项全过，新增排队消息编辑、移出；iOS 模拟器 `PhoneChatUITests`（演示数据）
  走行展示、diff 展开、编号选项回答、发送、插话标记、chip 切排队、排队卡片移出、停止后"继续"、会话列表、新对话表单。
  **没测**：真实被控端上的插话（当时本分支的被控端还没有 `caps.steer`；2026-10-22 已补，见下一条）、真实的计划 / 提问 / diff
  事件（替身智能体不发这些）、上下文用量环的真实数据。
- 2026-10-22（去掉单独的插话操作后）：`tools/test-phone-chat-model.sh` 17 项全过；`python3 tools/phone-chat-smoke.py`（安静模式）
  30 项全过，新增**真实被控端上的插话**：替身智能体 `caps.steer=true`，SLOW 轮次运行中 `sendMode` 自动为 steer，发出的消息作为
  用户行出现在同一轮、智能体在同一轮答 `steered: …`、队列为空；随后把 `caps.steer` 置 false 模拟 ACP 智能体，运行中发送进队列、
  编辑、移出、停止后排队消息接着跑。iOS 模拟器（iPhone Duo，iOS 27.1）`PhoneChatUITests` 两个用例都过（30 s / 12 s）：
  运行中发送的消息直接出现在对话里、没有 chip、没有"插话"标记、没有排队卡片；`-ui-chat-no-steer` 启动参数下运行中发送
  出现排队卡片、能编辑、没有"现在插进这一轮"、移出后卡片消失。**没测**：真机；真实 Claude / Codex 的插话（被控端那边
  已经在 Mac 和 win-dev 实测过，见"运行中插话"一节，手机端只连过替身）。

### 踩过的坑

1. macOS 上跑 debug 版 `libar_mobile` 会在 Swift 协作线程上栈溢出（`tracing::Instrument` 处 EXC_BAD_ACCESS）：debug 的
   Rust future 太大。手机用的是 release 框架；冒烟脚本用 `DYLD_LIBRARY_PATH=target/release` 换成 release 库。
2. 同一台机器上两个控制端（命令行和"手机"）昵称都取主机名，第二个配对会被拒（nickname already taken）；冒烟脚本给
   手机进程设 `HOSTNAME=smoke-iphone`。

3. 主机端的设计：没带第一条消息启动的会话是 parked（收到第一条消息才拉起智能体进程）。手机上原来显示"已休眠"容易误解，
   现在没有轮次的 parked 会话显示"等待第一条消息"。
4. 模拟器的 iOS 运行时要和 Xcode 的 SDK build 一致（本机 SDK 24A94232，`-downloadPlatform` 先下的是 24A94425，能列出但
   起不了设备）；`xcodebuild -downloadPlatform iOS -buildVersion 24A94232` 下对应版本后才能 boot。

### 下一步

- [ ] 手机控制端的设备名：iOS 上 `machine_name()` 取不到主机名，被控端把手机记成 `unknown`（对话里用户行下面显示
      "unknown"）。应由 App 传入设备名（`UIDevice.current.name` 或用户设置）给内嵌控制端。
- [ ] 等待机主同意（`chat_start` 走 ask）时的界面没在真实被控端上看过（测试被控端规则放行了 `chat_*`）。
- [ ] 问题（`question`）目前只显示，没有回答按钮（`{"answer":...}` 已有通道，`PhoneClient.chatSend` 可直接发）。

## 被控端托盘与“本设备”页（2026-10-21，分支 `feat/host-tray`，worktree `../ai-remote-tray`）

用户的痛点：被控端只有命令行，权限（屏幕录制 / 辅助功能）要靠 doctor 提示再手动去系统设置加；希望"做成软件托盘之类的，
可以随时打开相关设置"。

### 决定

- **形态选 A：托盘放在 Tauri 桌面端，不给 `ai-remote-hostw` 加原生托盘。** 理由写在 `docs/architecture.md` 第 8 节
  "The tray"：被控端以 LaunchAgent / 计划任务 / SYSTEM 服务运行，画不了托盘（或画到错误的桌面）；桌面端在登录用户的会话里，
  经被控端已有的本机控制管道（`control.sock` / 命名管道）读状态、发命令，不需要被控端的代码和权限。管道做不到的走
  `ai-remote` 命令：新加的 `ai-remote host start`，以及没有被控端运行时的 `doctor`。
- **控制协议只加字段、不改旧字段。** `HostRequest::Status` 的回复新增 `connected`（任一通道有连接的控制端名）、
  `controllers`（每个控制端开着哪些通道：tools / events / screen / files）、`calls`（正在执行的工具调用：谁、什么工具、
  是不是人在操作）、`last_call`（最后一次结束的调用和结束时间）、`driver`、`prompt_blocker`。新增 `Pause { on }`
  （本机暂停 / 恢复，审计里 `by: "host"`）和 `Recent { limit }`（审计尾部，复用 `AuditLog::subscribe` 的 300 行 backlog）。
- **"谁在这里"用独立的在场登记（`crates/ar-host/src/presence.rs`），不复用 `LiveConns`。** `LiveConns` 只登记 MCP 工具连接，
  吊销 / 遗忘时按它带原因断连；事件流、推流、文件三条通道不在里面，所以桌面端只开着观看页时托盘会以为"没人连着"
  （真机试出来的）。在场登记在四条协议接入时各登记一次、连接结束时由 guard 注销，工具调用在 `gate.rs` 第 5 步执行期间
  登记。它只用于上报，不参与任何判定，所以不改吊销语义。
- **托盘图标**：Tauri 自带的 `tray-icon` 特性（`tauri = { features = ["tray-icon"] }`，依赖本来就在 Cargo.lock 里，没有新 crate）。
  图标运行时用 `image` 手绘：品牌图形两块多边形（`glyphs.tsx` 的 PA/PY）4× 超采样后盒式滤波；macOS 44 px、Windows 32 px。
  右下角角标：空心环 = 有控制端连着（任一通道）；实心点 = 有人在操作（工具调用正在执行或 8 s 内刚结束，或有人接管）；
  两竖条 = 已暂停；无角标 = 空闲；整体变淡 = 被控端没运行。**macOS 用模板图**（只看 alpha，系统按菜单栏明暗着色，和其他菜单栏
  图标一致；第一版用了 `#8E8E93` 灰色非模板图，真机截图上明显比其他图标暗），状态只靠形状区分；Windows 角标上色（蓝 = 智能体、
  琥珀 = 人、橙 = 暂停）。注意 `TrayIcon::set_icon` 在 macOS 会把模板标记清掉，要用 `set_icon_with_as_template`。
- **"刚结束的调用也算在操作"**：智能体的调用大多不到 1 s，托盘 3 s 轮询一次几乎抓不到正在执行的那一刻，所以被控端报
  `last_call.ended_ms`，托盘把 8 s 内结束的调用也当作"正在操作"（`host_local::BUSY_AFTER_MS`）。
- **轮询而不是推送。** 托盘每 3 s 连一次控制管道取 `status`（本机 socket / 管道），菜单操作后 0.4 s 补一次；状态没变就不重画。
  "本设备"页的状态每 5 s 轮询一次，但 doctor（要读自启和防火墙状态，Windows 上慢；没被控端时还要起一个 CLI 进程）只在进页面
  和操作之后跑（`local_host(doctor)` 参数）。
- **关窗进托盘**只在两种情况下：本机有被控端在运行，或用户打开了"登录时打开"。只做控制端的电脑关窗口仍然退出。macOS 窗口收进托盘时
  隐藏 Dock 图标（`set_dock_visibility(false)`），点托盘菜单或 Dock（`RunEvent::Reopen`）再出来。
- **登录时打开（`login_item.rs`）默认关**，用户在"本设备 → 登录时启动"里手动开：macOS 写 `~/Library/LaunchAgents/io.github.jia-ss.ai-remote.app.plist`
  （`--hidden`，`LimitLoadToSessionType = Aqua`，不 `launchctl load`，下次登录生效），Windows 写
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 的 `ai-remote` 值（用 `reg.exe`）。没用 `tauri-plugin-autostart`：
  它最新版是给 Tauri 3 的 alpha，macOS 默认走 AppleScript 登录项会弹"自动化"授权。`--hidden` 启动时页面不调 `show()`。
- **托盘菜单语言跟页面一致**：页面启动时 `set_ui_language` 上报，之前按 `LANG` 猜。文案在 `tray.rs` 的 `status_line()` / `texts()`。
  中文里拉丁名字后面加空格（"laptop 已连接"），汉字后面不加（`gap()`）。
- **页面侧**："本设备"页新增 `HostPanel`：状态行（pid / 版本 / 启动方式、已连接控制端及其通道、当前操作者、正在执行的调用、
  审批弹窗是否受阻）+ 暂停 / 恢复、停止 / 启动；配对码弹窗（级别可选，点"生成"才铸票据，票据只在弹窗里显示，可复制，不写日志）；
  "权限与会话"组直接用 doctor 检查项，macOS 权限缺失时"打开系统设置"走
  `x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture` / `?Privacy_Accessibility`（只打开面板）；
  "登录时启动"组（被控端自启检查、未安装时的 install 命令、macOS 登录项面板按钮、本应用的"登录时打开"开关）；"审批规则"组
  （在访达 / 资源管理器里显示配置目录）；"最近活动"组（审计尾 20 行，`id="recent"` 给托盘菜单定位）。托盘到页面的跳转走 Tauri
  事件 `tray-navigate`（`App.tsx` 监听，payload 是 `TrayIntent`），不刷新页面。
- **机主暂停（`Turns::owner_pause` / `owner_resume`）比控制端的暂停更强**：智能体和接管者的调用都拒绝，控制端不能 resume / take_over
  （`OWNER_PAUSED` 的说明），控制端的 pause 不会覆盖它；恢复时回到暂停前的操作者（接管者暂停期间交还，则回到智能体）；本机恢复幂等。
  `Driver::Paused` 多了 `owner` 字段，审计行带 `"origin": "owner"`；控制端时间线、电脑页、会话页的恢复按钮都按它区分。画面仍可观看。
  审查前的第一版用的是控制端同款暂停，界面却写"所有控制端都被拒绝"，两边对不上。
- **配对码**：点"生成"才铸票据；显示期间 `set_content_protected(true)`（macOS `NSWindowSharingNone`、Windows `WDA_EXCLUDEFROMCAPTURE`，
  系统支持时对截屏 / 推流隐藏窗口，未在真机验证采集端是否真的排除）；有控制端连着时先提醒；复制只复制票据本身（配对页也接受带
  `ai-remote pair ` 前缀的粘贴）；被控端在配对时写一条 `paired` 审计（含三词安全码，它不是秘密），弹窗轮询 `recent` 显示对方名字和安全码，
  不一致可一键移除（`local_host_forget` → `HostRequest::Forget`）。
- **状态三态**：控制管道 NotFound / ConnectionRefused 才算"未运行"，其他错误（如服务模式管道拒绝访问）显示"在运行，但读不到状态"，
  关窗也保留托盘。旧版被控端的 status 没有新字段，前端全部按可选处理（第一版会直接白屏：这台 Mac 的 LaunchAgent 被控端就是旧版）。
- **Windows 服务模式**：status 带 `system`；`ai-remote host start` 遇到需要管理员的启动（`sc start`）且当前未提权时直接说明怎么办；
  托盘 / 页面停止服务前先确认（停了只有管理员能再启动）。没有给服务加 `sc sdset` 放开启动权限。
- `ai-remote doctor` 有 fail 时退出码为 1，`cli_json` 只要 stdout 是 JSON 对象就照用（否则没有被控端时整组权限都不显示）。
- 页面拿不到托盘命令：`capabilities/default.json` 显式 deny 了 `core:tray:*`（开 `tray-icon` 特性后 `core:default` 会带上它们）。
- "审批规则"定位到 `host/ai-remote.toml`（`open -R` / `explorer /select,`），不是配置目录根。

### 验证（2026-10-21，本机 macOS 26，debug）

- `cargo fmt --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、四个 `tools/*.test.mjs`（47 过）、`tsc --noEmit` 全过；
  `cargo test --workspace --locked` 全过，其中 `stream::pointer::mac::tests::the_current_pointer_can_be_read` 在一次整跑里失败、单独重跑通过
  （它前后读两次光标形状，中间光标变了就会失败；本分支没动这个文件）。新增单测：`presence::tests`、`turns::tests::the_owners_pause_…`、
  `control::tests::tray_requests`、`tray::tests`（角标、图标像素、中英文案）、`host_local::tests`（status 解析、刚结束的调用、未知状态）、
  `login_item::tests`。
- 真机（`/tmp` 下独立配置目录起新版被控端，同一配置目录的控制端与它配对；没碰 `~/Library/Application Support/ai-remote` 和 LaunchAgent 被控端）：
  托盘图标是模板图、和其他菜单栏图标同色；有控制端连着显示空心环，循环调用 `system_info` 时显示实心点、菜单写"studio-laptop 的智能体正在操作（system_info）"；
  托盘"暂停"后图标变两竖条、菜单变"恢复被控端"，控制端 `ai-remote resume` 被拒（"the owner paused this computer…"），智能体调用被拒并说明原因；
  托盘"打开设置"跳到"本设备"页。截图：`docs/images/desktop/tray-menu-{operating,paused}.jpg`（computer-use 截屏，裁到只剩本应用图标和菜单）、
  `tray-settings-{dark,light}.jpg`（浏览器预览 + demo 后端，无头 Chrome 截图：真机截图里会出现本机主机名，不进公开仓库）。
  "本设备"页在真机上也看过（权限、自启、规则、最近活动都正常显示），只是没有放进仓库。
- 多 agent 审查（7 个维度、每条两个角度复核；复核阶段一半因额度中断，剩下的由我逐条判断）找出的问题都已处理，主要是上面几条。
- Windows：只靠推送后的 windows-2025 CI 编译 / clippy / 测试，真机没跑托盘。

### 应用内一键开启与授权（2026-10-21 第二轮，用户硬要求：不再让用户敲终端命令）

- **开启 / 关闭**（"本设备"页、托盘"启动被控端"在未安装时也引到这里）：先用包内 CLI 跑 `host install [--service] --dry-run`
  （新加 `--json` 输出 summary / notes / steps / blocker）在弹窗里列出要做的改动，再执行。要管理员的走系统自己的授权框
  （`apps/desktop/src-tauri/src/elevate.rs`）：macOS `osascript -e 'do shell script "…" with prompt "…" with administrator privileges'`
  （管理员密码框，命令以 root 跑；PR #13 已让 root 下的 `--service` 安装不再等被控端），Windows 用 PowerShell
  `Start-Process cmd.exe -Verb RunAs -Wait`（UAC），输出重定向到临时文件读回。macOS 普通安装不用管理员，直接以用户身份跑；Windows 普通安装
  （最高权限计划任务 + 防火墙）和服务都提权。关闭时按 `host status` 的新字段 `service` 决定要不要提权。没选 SMAppService / Authorization
  Services：前者注册的是 app 自己的 helper，管不了登录窗口会话的系统 LaunchAgent；后者在 Rust 里要绑定已废弃的 `AuthorizationExecuteWithPrivileges`。
- **被控端在 app 包里**：macOS 包本来就带 `Contents/MacOS/ai-remote(-hostw)`（`tauri.macos.bundle.conf.json` 的 externalBin），安装计划用的是运行中
  CLI 旁边的 hostw，所以 app 调包内 CLI 时 plist 指向包内路径，`AssociatedBundleIdentifiers` 是 app 的 bundle id（实测：/tmp 测试配置装出来的 plist
  就是这样）。自启项指向别处（比如某个 build 目录或 `~/.local/opt`）时页面提示"改用这个应用"，一键重装。Windows 包还没把 CLI 打进去（见未做）。
- **提权前先以用户身份跑一次 dry-run**：全新配置目录如果第一次由 root 创建，host 目录会是 root 的，被控端写不了审计日志。
- **一键授权**（macOS）：权限行的"授权…"走控制管道新操作 `request_permission`，由**被控端自己**调 `CGRequestScreenCaptureAccess` /
  `AXIsProcessTrustedWithOptions(prompt)`（`crates/ar-host/src/platform/tcc.rs`），所以授权记在被控端（app 包里的 hostw）身上；远程调用到不了这条管道，
  "远程工具不触发 TCC"仍然成立。之后页面每 2 s 调 `permissions`：被控端起一个带 `AI_REMOTE_TCC_PROBE=1` 的自身子进程重新读授权（运行中的进程在
  屏幕录制授权后要重启才看得到），新进程有了而运行中的没有，就自动重启被控端（停止 + `host start`），再刷新检查。等 3 分钟没结果就提示去系统设置；
  旁边一直有"打开系统设置"深链兜底（系统只弹一次请求，拒绝过后就不再弹）。
- **真机从零走一遍（2026-10-21，release 包装到 `/Applications/ai-remote.app`，/tmp/ar-setup-test 全新配置）**：
  "开启远程控制…" → 计划弹窗（指向 `/Applications/ai-remote.app/Contents/MacOS/ai-remote-hostw`）→ "开启（需要输入密码）" → 系统管理员密码框 →
  `/Library/LaunchAgents/…eaa66997.plist` 写入，`host status` 为 `service: true, current: true`，被控端由 launchd 拉起。屏幕录制点"授权…"后用户在系统设置里打开，
  授权生效（新进程探测为 true）。**坑 1**：在系统设置里打开屏幕录制后，macOS 提示"退出并重新打开"，会结束这个 app 的所有进程——包括包里的 hostw；hostw 干净退出，
  launchd 只在崩溃后重启，于是被控端一直停着。已修：桌面端启动时若被控端已安装、没运行、且不是机主在应用里停的（`<profile>/desktop-host-stopped` 标记），就自动
  `host start`。**坑 2**：TCC 把授权记在 **app 包**（hostw 带 `AssociatedBundleIdentifiers`）上，ad-hoc 签名的身份是主程序的 cdhash，所以**每次重编桌面端，
  已有授权就失效**（实测：只换 `ai-remote-desktop`、hostw 的 CDHash 不变，新进程探测的屏幕录制从 true 变 false）；系统设置里仍显示旧条目为"开"，但对新版本无效，
  要选中它点"−"删掉再授权。页面现在会比较"运行中的被控端"和"新进程"的结果，前者有后者没有时标成"授权已失效"；授权超时的提示也写了删旧条目。根本解决要用固定身份
  签名（Developer ID，或至少一个固定的自签名代码签名证书，`APPLE_SIGNING_IDENTITY`），未做。
- **把真实被控端从 `~/.local/opt` 换到 `/Applications`（2026-10-21）**：坑 3：`bootout` 返回时 launchd 还没放掉旧任务，紧接着 `bootstrap` 同名标签报 `5: Input/output error`，安装计划已加"等待卸载完成（最多 10 s）"一步；坑 4：载入成功但 launchd 没启动它，要 `kickstart`，app 开启后现在会兜底启动；坑 5：直接 `ditto` 覆盖 `/Applications` 里正在用的包，下次启动报 `OS_REASON_CODESIGNING`，要先删掉旧包再拷。真实被控端现已运行在 `/Applications/ai-remote.app`（服务形态，`current: true`），两项授权仍待机主在系统设置里删掉旧条目后重新允许。
- **一键处理授权残留与被关闭的服务（2026-10-21）**："授权…"现在先用 `tccutil reset <服务> <本应用 bundle id>` 清掉本应用的旧条目（只动自己的），
  再由被控端请求，所以重编后不必去系统设置手动删条目。`host start`（页面"启动"、托盘、新加的"重新允许并启动"）在 macOS 上先 `launchctl enable`、
  未载入时 `bootstrap`、再 `kickstart`：真机上见过服务被标成 `disabled`（登录项开关）和被整个卸载两种情况。`host status` 多了 `disabled`，doctor 的自启检查会提示；
  app 启动时的自动拉起在 `disabled` 时不动作，开关是机主的。注意：从终端跑 `ai-remote doctor` 且没有被控端在跑时，权限检查的是终端自己的授权，不代表被控端。
- 实测（本机，/tmp/ar-tray-fresh 全新配置）：包内 CLI 普通安装 → plist 指向包内 hostw、带 `AssociatedBundleIdentifiers`、被控端启动；`permissions`
  探测返回 `probed: true`；关闭（普通权限）移除 plist、被控端停止。管理员通路：用同样格式的 osascript 跑无害的 `/usr/bin/id`，系统管理员密码框弹出
  （截图里是黑的：SecurityAgent 窗口不让截），有人在机器前输入密码后返回 `uid=0(root)`。**没做**：从零点按钮装 `--service` 并在系统授权框里允许
  两项权限、doctor 全绿——要机器主人亲自输密码、点授权，留给用户。截图 `docs/images/desktop/host-setup-{fresh,dialog,grant}.jpg`（demo 数据）。

### 未做 / 下一步

- Windows 打包：`tools/windows.ps1 build` 用 `tauri.windows.bundle.conf.json` 把 `ai-remote.exe` / `ai-remote-hostw.exe` 作为 sidecar 打进安装包
  （dry-run 在 win-dev 的 Windows PowerShell 5 和 pwsh 上都试过，CI 上有 `tools/test_windows_packaging.py`）；真正出一次安装包、在干净的 Windows 上点"开启"没做。
  UAC 通路：第一版 `Start-Process cmd /c` 遇到带空格的路径被拆成 `C:\Program`（win-dev 实测），改成提权 PowerShell + `-EncodedCommand` 后，带空格路径的输出和退出码
  都对（win-dev 上 agent 已提权，所以没弹 UAC 框本身）。
- macOS 授权在每次重编后失效（见上面坑 2）：已有做法——用自签名代码签名证书 `ai-remote local signing` 编（`APPLE_SIGNING_IDENTITY`，见 `docs/macos.md`），2026-10-22 本机已建证书、release 包三个程序都用它签名并通过 `codesign --verify --deep --strict`；装进 `/Applications` 后授权、重编后授权仍在，这两步还没验证（要替换真实被控端，等用户确认）。
- 从零开始（未安装）点按钮装服务形态 + 两项授权 + doctor 全绿，要用户本人在这台 Mac 上走一遍（输管理员密码、点系统授权框）。

- 没有系统通知（控制端连上、审批等待时弹一条）；审批本身已有自己的弹窗。
- Windows 真机没跑过托盘（只靠 CI 编译）；服务模式下控制管道允许安装者账户这一点已有真机结论，托盘走同一条管道。
- 桌面端没有单实例保护：登录项启动了一份，再从访达 / 开始菜单双击可能起第二份（两个托盘图标）。`tauri-plugin-single-instance` 可以解决。
- 服务模式下桌面端不能一键启动服务（需管理员）；要做的话在 `--service` 安装计划里加 `sc sdset` 给安装者 SID 开 SERVICE_START，并写进 `--dry-run`。
- 配对码窗口的防截屏在 macOS 15+ 的 ScreenCaptureKit 上是否仍被尊重、Windows DDA 是否排除，都没在真机验证。
- `docs/operations.zh-CN.md` 第 3 节开头仍写"从不做成系统服务"（本分支之前就有，未改）。
- 真机试的时候发现测试配置里 studio-laptop 被人在界面上设成了"全部放开"、本应用的"登录时打开"也被打开过（14:28，都需要点确认 / 选择，
  不是自动发生的）；登录项指向的是 worktree 里的 debug 包，已删除。
- "最近活动"是审计原文的句子化，不是会话页那种步骤卡；完整视图在"活动"页。

## 配对码：手机扫码、电脑输 8 位数字（2026-10-21，分支 `feat/short-pair-code`，叠在 PR #14 `feat/host-tray` 上）

用户原话："授权码能否搞的简单点？然后做的好看点，手机支持扫码，电脑才输入授权码这种"。以前要把约 1000 字符的
`mcpmesh-invite:` 票据从一端复制到另一端。设计调研（Magic Wormhole / AirDrop / RustDesk / Tailscale·iroh ticket 对比）
和安全分析写在 `docs/architecture*.md` 第 4 节"短码怎么换到票据"。

### 设计

- **配对码不是凭证本身，只是找到被控端并换出票据。** 票据照旧签发、照旧由 mcpmesh 兑换（同一个配对仪式、同样的三词安全码），
  旧的 `ai-remote pair <票据>` 完全不变。
- 8 位随机数字，显示为 `4821 0937`；输入时空格 / 横线 / 点 / 全角数字都认，`ai-remote://pair?code=` 链接也认（二维码内容）。
- **会合 endpoint**（`crates/ar-net/src/paircode.rs`）：两端 Argon2id（64 MiB、3 轮、固定盐）把码拉伸成 64 字节——前 32 字节是
  被控端临时 iroh endpoint 的私钥，后 32 字节是认证密钥。被控端在码有效期内跑这个 endpoint（中继 / 代理 / mDNS 跟节点同配置，
  mDNS 服务名 `ai-remote-pair`），只服务 ALPN `app/ai-remote/pair-code/1`；控制端从码算出 endpoint id，用一次性 endpoint 拨过去，
  发 `blake3::keyed_hash(auth, 两端 id)` 证明知道码，被控端回票据。
- **为什么另起 endpoint、不用节点自己的**：mcpmesh 的闸门只放行已配对设备，未配对的控制端到不了节点上的任何应用 ALPN；
  而且不能把节点身份发布在一个只有 27 位熵派生的 key 下。不需要改 vendor/mcpmesh。
- 限制：10 分钟（票据先过期则更短）、一次性、5 次证明失败即作废（`Offer::answer` 状态机）；用掉 / 作废 / 过期后 endpoint 立即关闭。
  同一时间每个被控端只有一个码，新码替换旧码。
- 被控端控制通道：`PairCode` 应答新增 `code {code, grouped, link, expires_at}` 和 `qr {size, path}`（`ar-host/src/pair_qr.rs`
  用 `qrcode` crate 画 SVG path / 终端方块字符）；起不来码时返回 `code_error`，票据照常给。新请求 `WithdrawCode`（配对窗口关闭时撤码）。
- 控制端：agent 的 `pair` op 先 `PairCode::parse`，是码就 `Mesh::pair_code`（取票据再兑换），否则当票据。CLI / 桌面 / iOS 都走这一个入口，
  CLI 的 `pair` 接受多个词（`ai-remote pair 4821 0937`）。

### 界面

- 桌面被控端：`apps/desktop/src/ui/PairCode.tsx` 的 `PairCodeDialog` 替换了 PR #14 在 `Settings.tsx` 里的同名对话框（入口不变：
  "本设备 → 显示配对码"、托盘"Show Pairing Code"）。打开即出码；左边白底二维码（任何主题下都深色码点在浅底上，扫码器要这样），
  右边大号数字、倒计时进度条、"换一个"、级别下拉（改级别即换码）；完整票据收在"两台设备之间不通网络？使用完整票据"里。
  有设备配对后切到三词核对页（一致 / 不一致并移除）。关闭未配对的窗口会撤码。演示后端 12 s 后模拟一台 iPhone 配上，方便看核对页。
- 桌面控制端：`Pages.tsx` 的 `PairPage` 第一步改成一个大号数字输入框（自动分组，粘贴票据自动切到票据模式），"改用票据"链接。
- iOS：`PhonePairing.swift`（扫码 + 输入 + 票据入口）、`PhonePairCode.swift`（与平台无关的码解析，`swiftc` 在 macOS 上能跑）。
  输入框在 iOS 上是 `PairCodeField`（包 `UITextField`），在 `shouldChangeCharactersIn` 里同步过滤、分组后写回。原来用 SwiftUI `TextField` + `.onChange` 事后改写，
  快速输入时改写和下一个按键抢，CI 上 `typeText("1234567")` 丢了一位（得到 `1234 56`）。本机 iOS 26.2 / iPhone 17 Pro 上该测试连跑 5 次、全部 15 个 UI 测试都过。
  扫码用 AVFoundation（`NSCameraUsageDescription`），拒绝权限时给"打开设置"；`CFBundleURLTypes` 注册 `ai-remote` scheme，
  系统相机扫到二维码直接打开 app 并配对（`PhoneStore.openPairLink`）；`NSBonjourServices` 加 `_ai-remote-pair._udp`。

### 实测

- `cargo test -p ar-net --lib paircode`：码解析、生成、派生、猜测上限、过期、一次性，加一个两个 hermetic endpoint 走回环的完整交换。
- Argon2id 64 MiB×3：Apple M5 Max release 78 ms / 次（`kdf_cost`，20 次，`--ignored` 手动跑）；先用的 19 MiB×2 是 12.7 ms，
  离线穷举只要约 2000 核，所以调高了。dev 构建里 argon2 / blake2 设了 opt-level 3，否则测试很慢。
- 本机两个临时 `--home` 之间：`ai-remote pair 2545 3956` 2.7 s 配上；同一码再用报"没有设备响应"（会合 endpoint 已关，约 20 s 超时）。
- **跨机器（2026-10-21，Mac 控制端 ↔ win-dev 被控端，debug 构建，两边都是临时 `--home`；两台在不同网段 10.1.120.x / 10.1.77.x，
  所以走的不是 mDNS，是 DNS/pkarr + 中继/直连）**：7 次尝试——5 次成功，耗时 2.9 / 3.2 / 3.9 / 5.8（含自动改名重试）/ 3.5 s，
  两边显示的三词一致；1 次是故意用已被新码替换的旧码，10.4 s 后报"没有设备响应"（符合预期）；**2 次未解释的失败**：被控端刚启动后
  约 26 s 和 54 s 时用它启动时打印的码，各 10 s 报"没有设备响应"，约 3 分钟后同一个码用测试直连 1.08 s 就通了。当时 win-dev 上另一个会话
  在做 release 编译（CPU 很忙），怀疑是会合 endpoint 的 pkarr 发布被拖慢，没有证实；之后重启被控端、10 s 内用启动码配对（第 3 次）是成功的。
- **昵称冲突**：第一次重复配对时 mcpmesh 报 `nickname ... is already taken`（被控端还留着这台 Mac 上一次的配对）。票据流程里
  用户可以改名再兑换，配对码流程里码已经交出票据、不能再用，所以 `Mesh::pair` 现在遇到 `ERR_NICKNAME_TAKEN`（mcpmesh 保证此时票据
  未消费）就临时把自己的昵称改成 `<名字>-<设备 id 前 4 位>` 重兑一次（真机第 5 次就是这样配上的，被控端列表里是 `...-35db`）。
- iOS：只过了 macOS 上对全部 `Sources/*.swift` 的 `swiftc -typecheck`（用本机生成的 UniFFI 绑定）；`#if os(iOS)` 的扫码和 UI 测试靠 iOS CI。

### 下一步

- [ ] 被控端刚启动时的两次失败没查清（见实测）。可以给控制端加一次 3 s 后的重拨，或在被控端确认 pkarr 发布完成后再显示码。
- [ ] 输错码要等约 10 s（拨号报错）到 20 s（超时）才报"找不到"。可以缩短拨号超时，或在码里加校验位（会损失约 3.3 位熵，安全分析要跟着改）。
- [ ] 被控端在没有网络、只能用票据时，配对窗口只显示"无法生成配对码"；可以考虑把二维码换成票据本身（约 100+ 模块，要实测能否扫）。
- [ ] Windows 托盘入口与 iOS 扫码没有真机试过（本机无 Xcode）。

## 接管时的实时输入、剪贴板与文件（2026-10-21，分支 `claude/live-input`）

用户反馈："帧率不够高，控制卡卡的，鼠标点击拖动不太行；剪贴板复制、文件复制也要适配。"

### 根因

- **操作卡、拖动不行**：`RemoteInput.tsx` 原来只在松开鼠标时发一次 `mouse` 工具调用（click 或"起点→终点"的 drag），
  移动过程中什么都不发；每次调用经 controller agent → QUIC → 闸门 → 输入互斥锁 → 30 ms 悬停暂停，还排在 32 项队列里。
  远端指针在松开前根本不动，拖选、拖窗口、滑块都做不了，连点快了就 `input busy`。
- **帧率**：推流本身是满的（实测见下）。"不够高"来自两点：默认"均衡"档只请求 60 fps / 1920 长边，而这台 Mac 的屏幕是
  100 Hz、被控端是 5K；以及上面的输入延迟。查过 WKWebView 是否把页面刷新锁在 60：WebKit 有 feature flag
  `PreferPageRenderingUpdatesNear60FPSEnabled`（默认开，可经 `-[WKPreferences _setEnabled:forFeature:]` 关），但在这台
  100 Hz 外接屏上开 / 关 rAF 都是 83.7 fps（可见窗口，各 3 s），**不是瓶颈，没改**。

### 做了什么

- **被控端 ALPN `app/ai-remote/input/1`**（`crates/ar-host/src/input_channel.rs`，协议见文件头注释）：JSON 行流，
  移动 / 按键 / 滚轮 / 键名 / 文本 / 剪贴板 / 全部松开；回传剪贴板变化、拒绝、暂停。
  - 准入：必须是**已接管的那台设备**（`turns` 的 `Driver::Person{device == peer.name}`），control 级或 bypass，
    `mouse`、`keyboard` 策略都是 allow（deny / ask 拒绝整条流，应用退回逐条工具调用）。剪贴板读写分别看
    `clipboard_read` / `clipboard_write`（allow 才同步）。**每秒复查一次**（交还、暂停、别人接管、降级、bypass 到期 /
    关闭、策略变化 → 1 s 内结束，`input_end.reason` 写原因）。被拒写 `input_refused`；同时最多 4 条流。
  - 注入：一条线程（macOS 多击计时靠它），批内连续移动只留最后一次（`pending` 按收到的条数扣，含被合并的）；
    每次按下前做工具同款检查；`keys::forbidden` 照拒；Ctrl+Alt+Del 有服务时先松开为它按住的键再走 SAS；
    已按住的键再按 = 自动重复；被控端布局打不出的字符（é 等）在无快捷修饰键时按文本输入；松开前先跟随输入桌面，
    发送成功后才从按住列表删除（失败留到收尾重试）；暂停（审批弹窗 / 桌面不可用）时松开全部、移动静默丢弃；
    **3 s 没有任何行就松开全部**（应用每秒发 `{"hb":1}`）；超过上限的行跳过并报错，不断流（上限 = 6×1 MiB + 64，
    JSON 转义最坏 6 倍）。
  - 剪贴板：轮询 400 ms，**第一次只记基线不发送**（两边都是，接管不会互相覆盖）；Windows 先看
    `GetClipboardSequenceNumber` 变了才打开剪贴板（每 400 ms OpenClipboard 会让用户自己的复制粘贴偶尔失败）；
    锁屏 / 安全桌面上不读不写；控制端的写入排成一个队列串行执行，写入中轮询不读（否则读到旧内容当成新变化回发）；
    回显标记在剪贴板变成别的内容后清掉；文件列表变空也发 `clip_files: []`。
- **controller agent** 新 op `input`（`agent.rs` 的 `open_input` / `LiveInput::relay`，每次写都 flush）。
- **Tauri**：`open_input / send_input / close_input`；**`send_input` 是同步命令 + 无界队列**（异步命令并发执行，两批
  4 ms 间隔的输入可能乱序；有界 `try_send` 会丢掉松开键 → 卡键），积压超过 64 批时只丢纯移动批次。
  `clipboard_text / set_clipboard_text / clipboard_files`（arboard）、`push_file / pull_file`（`src-tauri/src/transfer.rs`，
  与命令行同一套票据）：推送不覆盖远端同名文件（`files_stat` 探测后加 `-1` 后缀，`overwrite:false`），拉取的文件名
  去掉盘符 / 非法字符后才落到"下载"。
- **前端**：`lib/live-input.ts`（4 ms 批、移动合并、滚轮像素→格数〔WebView2 100 px/格，Mac 50〕、心跳、键名映射：
  ⌘↔Ctrl 跨平台互换，按住 Ctrl/⌥/⌘ 时字母数字按 `code` 键位命名，AltGr 本身不发）；`RemoteInput.tsx`：按
  `PointerEvent.buttons` 同步多键（第二个键按下只来 pointermove），e.repeat 发重复按下，Mac CapsLock 每次事件是一次
  轻按，⌃⌘F 不转发，Meta 抬起只放非修饰键，流断开自动重开 3 次再退回逐条调用，切回应用立即读一次本机剪贴板，
  剪贴板按 UTF-8 字节限 1 MiB；`Session.tsx`：拖放 / 剪贴板文件传输排队，错误和通知条同时显示；`lib/drop.ts`。
- **文件工具 `~`**（`files.rs` 的 `expand_home` + `platform::user_folders`）：`~` 是被控端**登录用户**的主目录，
  以 SYSTEM 跑的服务模式下经 `WTSQueryUserToken` + `SHGetKnownFolderPath` 取会话用户的；`~/Desktop` 是该用户实际
  看到的桌面（Known Folder，OneDrive / 重定向也对）；分隔符按平台拼。
- **呈现**：`video.ts` 每次显示刷新只画最新解码帧（`requestAnimationFrame`，20 ms 定时器兜底，二者互相取消；解码器
  切换时丢弃未画的旧帧）。HUD 的 fps 改为呈现帧率，悬停看解码帧率。
- **默认画质"自动"**（`stream-quality.ts` 的 `resolveQuality`）：fps = 启动时用 rAF 测的本机刷新率（吸附到常见值，
  60~240，没测到按 120），长边 = 本机屏幕设备像素宽度夹在 1920~3840，40 Mbps（自适应码率往下调）。存过档位的用户不变。

### 审查（工作流，5 个维度各一个审查者，每条结论 2 个独立反驳者）

50 条，34 条两票都确认。全部确认项已修（上面列的大多来自它），未修的："?" 票未一致的里，两条流拼出被拒组合键 /
与工具路径不共用 `INPUT` 锁（同一设备才能开流，且需接管，风险低）、开流前的输入走工具调用可能与之后的实时事件乱序
（只在开流那 100~300 ms）、积压时丢弃的是最新移动。

### 实测（2026-10-21）

- 本机 Mac：`python3 tools/live-input-smoke.py` **22 项全过**（未接管被拒并审计、接管、策略 deny 拒、开流不同步剪贴板、
  `clip` 写入且不回显、被控端复制后 1 s 内收到、锁屏组合键 / 坏按钮 / 超限滚轮被拒、交还后流结束并带原因、
  `input_end` 计数）。会真的动一下本机指针并按一下 shift。
- **真机 Windows**（这台 Mac → win-dev，5120×2160@100Hz，直连 RTT 4.2 ms；新版本在临时 worktree 里出 release 包，
  以隔离配置跑一个用户态被控端；PowerShell WinForms 记录窗口记事件，`GetAsyncKeyState` 查卡键）：
  - 点击、Shift+H、文本、中文（Unicode 包）、Backspace 自动重复连删、Ctrl+A、右键、双击、滚轮（3 格 = −360、2 格 = +240）
    全对；**拖动：按住期间 19 个移动事件、松开在终点**。
  - 被控端开着中文输入法时，逐键发送的字母会进远端输入法（`ProcessKey`），和在那台机器上用实体键盘一样；本机有输入法
    时合成结果按文本发，不受远端输入法影响。第一轮拖动"丢失"就是远端输入法候选框挡住了按下点，关掉输入法后正常。
  - 卡键：Ctrl+Shift 按住时关流 → 都松开；按住 Ctrl 后静默 → 1.5 s 时仍按住、5 s 时已松开（流还开着）。
  - 剪贴板：Win→Mac 文本（含中文）、Win 剪贴板文件列表、Mac→Win 文本都到；开流时不同步。
  - 文件：命令行 push 到 `~/Desktop/` 再 pull 回来 SHA-256 一致。**服务模式（SYSTEM）下 `~` = `C:\Users\sunjiashuai`、
    `~/Desktop` = `C:\Users\Administrator\Desktop`**（这台机器把该用户的桌面重定向到了那里，和用户会话里
    `GetFolderPath('Desktop')` 一致）——临时把已装服务的 binPath 指向新包测的，测完已还原（`host status` 正常）。
  - Windows 上 release 构建、`cargo clippy -p ar-host -p ai-remote-desktop --all-targets -D warnings`、相关单测都过。
- 推流：win-dev → Mac 桌面包（离屏 smoke）均衡 解码 60 / 呈现 53~60 fps、延迟 21~27 ms；极致 5120×2160 解码 83~88 /
  呈现 51~62 fps、延迟 28~33 ms，0 丢帧（单次 6~12 s；离屏时 rAF 受限，呈现数偏低）。CLI 极致 89 fps、采集→到达
  p50 16 ms。
- **没测的**：桌面应用里用真鼠标拖动 / 拖放文件 / 点"保存到下载"的手感（computer-use 无法把测试窗口拉进用户正在用的
  全屏 Space，没测）；Windows 控制端（WebView2）的滚轮步长和 Ctrl→⌘ 映射；macOS 被控端的 `~/Desktop`（降权后同用户，
  理论上同 `directories`）；自动画质在 Windows 控制端的刷新率测量。

### 端到端延迟：和 zremote 对比、编码器最快档、控制端各跳（2026-10-21）

用户问"技术上做到极致了吗，zremote 的硬件编解码推流好像很快"。方法（`tools/latency-g2g/`，可复现）：被控端开一个
Edge 页面，每次刷新把本机毫秒时钟画成 40 个黑白块（32 位 + 8 位校验）；Mac 上的 Chrome 页面从**解码出来的帧**里读条码，
和本机时钟比（两机时钟差用 UDP 1000 次往返取最短那次，误差 ≤ 半个往返，实测 ±1.5 ms）。"ours" 模式经控制端 agent
拉我们的流（脚本中转，不经桌面端）、WebCodecs 解码；"zremote" 模式按 zremote 看板同样的设置走 WHEP（jitterBufferTarget
= 0）。量到"解码完成"为止，因为测量窗口不在用户正在看的 Space 里时浏览器不给 rAF，量"画到屏幕"会被节流污染。
**绝对值包含被控端 Edge 页面自己的渲染 + DWM 合成 + 采集等待（两边都有，约 30~40 ms，推算）**，所以只能横向比。

| 配置（win-dev 5120×2160@100Hz → Mac，直连，每组 20 s、约 180 个样本） | 组数 | 中位 | p90 | p99 |
|---|---|---|---|---|
| zremote：ffmpeg WGC + NVENC HEVC p1/ull、原生 60 fps 16 Mbps、mediamtx WebRTC | 4 | 62~72 ms | 73~91 | 82~344 |
| 我们同配置（HEVC 原生 60 fps 16 Mbps），被控端是装成服务的旧包（SYSTEM → DDA 采集） | 4 | 55~61 ms | 67~71 | 86~116 |
| 我们默认"自动"（3840 / 100 fps / 40 Mbps），同上旧包 | 2 | 46~48 ms | 61~63 | 73~85 |

编码器最快档 A/B（同一个新包、用户态被控端〔WGC 采集〕，只改 `AI_REMOTE_ENCODER_QVS`，交替跑）：

| 配置 | QualityVsSpeed 0（新默认） | 33（旧默认） |
|---|---|---|
| HEVC 原生 60 fps 16 Mbps，中位 | 40.2 / 40.1 / 39.6 ms | 43.2 / 43.4 ms |
| 自动（3840 / 100 fps），中位 | 40.0 / 37.5 / 33.3 ms | 37.5 / 44.6 ms（后一组受干扰，帧率掉到 80） |

5K 稳定低约 3 ms（解码也从 3.5 降到 2.5 ms：码流更小）；4K 档差异在噪声内、没有变差。所以默认改 0。
注意：同配置下这组（用户态、WGC）比上表的服务包（SYSTEM、DDA）低 15 ms 左右，**DDA 采集可能比 WGC 慢**，没单独 A/B（下一步）。

控制端各跳（真实桌面端 debug 包，`AI_REMOTE_TRACE_HOPS=<dir>` + `tools/hops-report.py`；页面侧时间在 WKWebView 里只有
1 ms 分辨率、和原生时钟差零点几毫秒，所以个别跳出现小负数）：

| 跳 | p50 | p90 | p99 |
|---|---|---|---|
| agent：QUIC 读完 → 写进 socket | 0.23~0.47 ms | 0.3~0.6 | 0.5~1.5 |
| socket → Tauri 原生侧 → 交给页面 | ≈0.05 ms | 0.05 | 0.1 |
| 原生 → 页面收到（IPC） | ≈0 | ≈0.5 | 0.9~1.8 |
| 页面收到 → 解码完成（WebCodecs HEVC 3840×1620） | 3~4 ms | 4~5 | 5~8 |
| 解码完成 → `drawImage` | 5 ms | 10 | 13~16（窗口不可见时靠 20 ms 定时器） |
| 单次 `drawImage` 耗时 | 0.01~0.03 ms | | |

结论：**跨进程整条链（agent → 页面）中位 < 1 ms、p99 ≈ 2 ms，不值得合并进程**；画布绘制本身几乎免费。显示路径
（换 `<video>` / 原生图层）这次**不换**：数据里看不到瓶颈，画布只多"等下一次 rAF"（100 Hz 下平均约 5 ms），换了也要等刷新。
"可见窗口里画布是否比原生图层多一帧合成"没测到（测量时用户在全屏应用里，窗口拿不到 rAF：`raf: 0, hidden: true`）。

### 踩过的坑

1. 被控端开着中文输入法时，测试里的逐键输入会被远端输入法吃掉、候选框还会挡住鼠标——测按键映射要先关掉目标控件的
   输入法（WinForms `ImeMode = Disable`）。
2. Tauri 异步命令并发执行，不保证调用顺序；要有序就用同步命令。
3. 在 PowerShell 里给 `sc.exe config binPath=` 传带双引号的值，内层引号会被吃掉；用 `cmd` 的 `\"` 转义才能原样还原。
4. 滚轮格数一开始在 flush 时才入队，会排到同批按键之后——改为累积满一格就按顺序入队。
5. `RemoteInput.tsx` 的 React 合成事件没有 `isComposing`，用 `nativeEvent?.isComposing`；测试夹具（`desktop-state`、
   `manual-input`、`stream-ui`）是依赖白名单，新依赖要补桩。
6. macOS 打 tar 会带 `._*` 元数据文件，传给 Windows 前设 `COPYFILE_DISABLE=1`。
7. macOS 上 Python 3.9 的 `TemporaryDirectory` 没有 `ignore_cleanup_errors`，跑 `stream-e2e.py` 要垫一层，并设
   `TMPDIR=/tmp`（Unix socket 路径长度）。

### 下一步

- [ ] 有人在 Mac 普通桌面 Space 下用桌面应用接管 win-dev，手动拖窗口 / 拖选文字 / 拖放文件 / 点剪贴板文件按钮各一次。
- [ ] Windows 控制端（WebView2）实测：滚轮一格是否正好一格、Ctrl→⌘、自动画质测到的刷新率。
- [ ] 剪贴板图片（arboard 支持 `get_image`）、多文件 / 文件夹传输。
- [ ] iOS 观看端接实时输入（现在只有桌面端）。
- [ ] DDA 与 WGC 采集的端到端 A/B（服务模式用 DDA，上面两组相差约 15 ms，原因未定）。
- [ ] 在普通桌面 Space 下用 `AI_REMOTE_SMOKE_VISIBLE=1` 跑一次可见窗口的"解码→上屏"，决定显示路径要不要改原生图层。

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
- **全屏接管时唤不出菜单（2026-10-21 用户反馈"全屏后无法唤出改分辨率、交还控制"）**：demo 里上下边缘 56 / 72 px 触发区
  本身能唤出，问题在三处：①接管后画面中间移动不触发，也没有任何可见入口；②鼠标停在浮条上 3 s 照样收起，画质下拉展开时
  页面收不到 mousemove，3 s 后浮条连同下拉一起被卸载；③全屏下提示 / 报错条（z 12，top 16）正好压在顶部浮条的画质选择上，
  报错条一直挂到下一次操作。改法：接管期间底部正中常驻一个 48×16 小把手（悬停 / 点击只会唤出浮条，接管时底部浮条抬到
  `bottom: 26px` 在它上方）；把手**常驻不消失**是故意的——若浮条出现时把手淡出，按原位置点下去会点穿到远端（底部正中就是
  Windows 任务栏）。计时到点时 `.nt-anchor:hover` 或 `.nt-anchor select:focus` 还在就续期，选完画质 `blur()` 释放；
  全屏下提示 / 报错条挪到 `top: 60px`。实测：Chromium headless shell + CDP 真实鼠标事件（`:hover` 生效），静止 3.6 s 收起、
  悬停把手出现、停在上 / 下浮条 4.5 s 不收、移回中间 3.6 s 收起；camofox 无头拿不到 `:hover`，只测了 select 焦点续期。
  **真机（2026-10-22）**：用户在 macOS debug 包（main 8eeabb6 + 下面两处鼠标修复）里接管 win-dev、原生全屏，手动走完清单：
  静止后浮条收起、把手常驻；悬停把手唤出两条浮条；画质下拉打开时浮条不收、选完后收起；推到屏幕底边 / 顶边时 Dock、
  菜单栏没有挡住浮条或把手；从底部浮条交还控制、把手随之消失，各 1 次，全部正常。**未测**：Windows 控制端（WebView2）。
- **全屏接管时鼠标消失（2026-10-22，上面那轮真机测试里发现）**：鼠标移到远端画面上就消失，黑边上能看到。两层原因：
  ①舞台在"全屏 + 接管 + 浮条收起"时整个 `cursor: none`（`.nt-stage.hidecursor`），黑边、把手、"视频已断开"层上都没有鼠标——
  已删，舞台上一律显示本地鼠标；②画面上远程输入层只要被控端单独发光标（`cursor: "client"`）就 `cursor: none`，不管远端光标
  画没画出来。临时诊断日志实测：win-dev 发来的 99 次位置全是 `visible: false`、`shape: 0`（位置照常变化），远端光标层一直
  `display: none`；在 win-dev 用户会话里 `GetCursorInfo` 得到 `flags=2`（`CURSOR_SUPPRESSED`：Windows 认为最近的输入来自
  触摸 / 笔，自己屏幕上也不画鼠标），所以被控端如实报告"不可见"。改法：`PointerLayer.move()` 返回这次画没画，`Stream`
  只在变化时通过 `onPointerShown` 上报，断流 / 卸载时报 false；`RemoteInput` 的 `remotePointer` 改成"远端光标正画在画面上"，
  只有这时才隐藏本地鼠标。win-dev 为什么一直处于被抑制状态没查（可能是别的会话注入过触摸 / 笔输入）。
  测试配方：debug 包不设 `AI_REMOTE_HOME`，直接复用正在跑的真实控制端（已是安静模式、已和 win-dev 配对），`AI_REMOTE_CLI`
  指向安装版的 `ai-remote`，这样不起任何新的被控端或控制端；`localStorage["ai-remote.video-trace"]="1"` 时每秒的 stats 行
  会写进 app 的 stdout。
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

- [~] 极致档全屏下 UI 重绘滞后 5~10 s：画布 5K@75fps 的 `drawImage` 压满合成器。2026-10-21 改为每次显示刷新只画最新
      一帧（见"接管时的实时输入"一节），应有缓解，未肉眼验证；仍不够的话再考虑 `OffscreenCanvas` + worker。
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

**延迟指标**（被控端在 `61d66ba`，观看端本轮）：帧头 `ts_us` 是按流时钟的采集时间；观看端发 `{"ping":n}`，被控端回
kind-3 带流时钟。桌面端 `lib/clock.ts` 取 60 s 内往返最短的一次做映射（NTP 规则，误差 ≤ 半个往返），会话页指标显示
"采集后 N 毫秒显示（±误差）"（解码出帧绘制时刻 − 采集时刻，中位数/秒）。命令行 `ai-remote view` 报"采集→到达"分位数。

**自适应码率**（`apps/desktop/src/lib/rate.ts`，被控端 `{"bitrate_kbps":n}` 在 `61d66ba`）：
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
  稳态 PSNR 0 档比默认低 0.06~1.0 dB，66 档高 ≤0.2 dB。当时保持默认；**2026-10-21 起默认改为 0 档**（端到端实测见
  "接管时的实时输入"一节的"端到端延迟"，5K 中位低 3 ms），`AI_REMOTE_ENCODER_QVS=<0..100>` 可覆盖。
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
# 网络设置：隔离 host/controller，两端切仅限局域网、换端口、手动设地址、quiet 锁（先 cargo build -p ai-remote）
python tools/network-e2e.py
# 网络设置页（演示数据，camofox REST，截图在 target/network-ui）
python tools/network-ui-smoke.py
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
      （9143cdb 改为 GENERIC_ALL，RustDesk 同），修后锁屏上键鼠都通（见"真机结果"）。
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

### 真机结果（2026-10-20，Windows 11 开发机，PR #11 分支 + a70a95d）

通过 zremote 的提权会话在开发机上执行，Mac 作控制端（Mac 10.1.120.x，开发机 10.1.77.x，不同网段）。

- `host install --service --dry-run` 与文档 3.2 节一致；真装一次成功：服务 pid 在 session 0，
  `ai-remote-hostw` 以 `NT AUTHORITY\SYSTEM` 跑在 Console 会话（session 1），`host status` / `doctor` 全绿
  （relay 警告：公司网无外网）。`CreateProcessAsUserW` 这段第一次跑就对。
- Mac 配对后 `hosts` 报 `path: direct`（跨网段直连，无中继）；`shot`、`info`、`doctor --host` 正常。
- `keyboard ctrl+alt+del` 走 SendSAS 生效：`info` 变 `input_desktop: Winlogon, secure_desktop: true`，`doctor` 的
  `desktop` / `approval` 按设计变 warn，**安全桌面截图正常**（DDA 在 SYSTEM 下采到 SAS 菜单）。
- **安全桌面上的注入失败**：`mouse move` 报 `拒绝访问 (os error 5)`（SetCursorPos），`keyboard enter/escape` 报
  `not all input events were sent (UIPI)`。改成在全新 OS 线程上注入（a70a95d）后仍失败，host.log 没有
  `cannot switch` 的 warn。对照实验：用被控端的 `shell` 工具（同一个 SYSTEM 令牌、同一 Console 会话）跑一段
  P/Invoke 脚本，在新线程里 OpenInputDesktop(GENERIC_ALL 或本项目的标志) → SetThreadDesktop → SetCursorPos →
  SendInput **全部成功**，桌面名 Winlogon；以用户身份跑则 OpenInputDesktop 就是 err 5。所以系统层没问题，
  差异在被控端进程内部，未定位。2026-10-21 补一条（eb395f9 把切桌面日志提到 info）：被控端日志里确实有
  `input thread switched desktop from="Default" to="Winlogon"`，紧接着 SendInput 仍报 os error 5——
  **线程已经在 Winlogon 上，注入照样被拒**。那么差异不在线程所在桌面，而在进程本身（对照脚本是被控端的子进程，
  却能成功）：候选是被控端进程的 integrity level / 令牌属性（服务用 DuplicateTokenEx + TokenSessionId 造的令牌
  和子进程继承的是否完全一致）、UIPI 相关的进程属性、或 enigo 事件里的 dwExtraInfo。
  **2026-10-21 定位并修复**：在失败点打印上下文（4d41d98，`platform::input_context`）——线程桌面 Winlogon、输入桌面
  Winlogon、本进程和前台窗口都是 System 完整性，全部对称；但在**同一个线程**上紧接着用 `GENERIC_ALL` 重新 OpenInputDesktop
  + SetThreadDesktop 后，SetCursorPos 和 SendInput 立刻成功。差异只在桌面句柄的访问权限：原来沿用采集侧的标志集
  （没有 `DESKTOP_JOURNALPLAYBACK`），SendInput 对安全桌面要求这个权限；改成 `GENERIC_ALL`（9143cdb，RustDesk 同）后，
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


## macOS 登录窗口交接：快速用户切换 / 注销（2026-10-21，分支 `fix/mac-login-window-handoff`）

**问题（真机，macOS 26.5）**：系统级 LaunchAgent（`[LoginWindow, Aqua]`）每个图形会话各起一份。快速用户切换
（用户菜单 "Login Window…"）时用户会话在后台继续运行，Aqua 那份一直占着配置目录（控制套接字的 flock），
`session_state` 报 "not in the macOS console session"；LoginWindow 那份降权成同一用户、抢同一把锁，拿不到就以 3
退出、被 launchd 约每 10 s 拉起一次，登录界面永远没人服务。Apple DTS 的说法：不能把 agent 限制在控制台会话里，
应由每份自己检测是否在控制台，不在就静默。

**决定**（逻辑在 `ar_host::handoff`，纯函数加单测；进程部分在 `ai-remote-hostw` 的 `system_agent`；只在
`--owner-uid` 时走，Windows、用户级 LaunchAgent、终端 host 都不变）：

- 规则一条、两边对称：**只有自己会话在控制台上的那一份持有配置目录；离开控制台的那份带着锁退出**。
- 等待不退出：`decide(Situation)` → `NotOurs | Stopped | Wait(OffConsole 2 s | Taken 1 s) | TryClaim | Serve`；
  拿到锁后再判一次（堵"对方先写 stop 标记再放锁"的竞态和切换中途的变化）。`WaitLog` 只在状态变化时写一行。
- 持锁时 `watch_console` 每 1 s 读 `kCGSSessionOnConsoleKey`，连续 2 次离开才 cancel `HostOptions::step_aside`；
  `run_claimed` 改成返回 `Stopped { reason, claim }`，hostw 在**仍持有 Claim** 时 `process::exit(4)`
  （`EXIT_STEPPED_ASIDE`）：flock、redb 的 peer store 锁、iroh 的 UDP socket 由内核同时释放。进程内不重跑
  `run_claimed`，因为 Mesh/HostState 有 Arc 环、`Mesh::shutdown` 没被调用，redb 锁在进程内放不掉。
- `on_console` 为 `None`（读不到会话字典）时：LoginWindow 那份（launchd 以 root 起的，`login_window_instance()`）
  算在控制台上，Aqua 那份算不在——避免降权后读不到字典导致登录界面没人服务，又不让后台会话凭猜测占锁。
- 退出码：3 = 已有 host（非系统 agent 路径，不变）；4 = 让位（launchd 会重新拉起，新进程在门禁里等）；
  143 = SIGTERM（持锁时先 `stopping.cancel()` 等 300 ms 发 close 帧；等待时直接退出）；0 = stop / 别的用户的会话。
- `getuid() != owner_uid` 且不是 LoginWindow 那份（=别的用户的 Aqua 会话）：记一行、以 0 退出，不碰配置目录。
- `stop-host` 仍是停止：`HostRequest::Shutdown` 在 cancel **之前**写 `<profile>/host/stopped`
  （`{boot: kern.bootsessionuuid, asid: getaudit_addr 的 ai_asid, at}`，临时文件 + rename）。生效条件
  `honored()`：同一次开机，且（同一个 asid，或本份是 LoginWindow 那份）。即同会话（锁屏、切回）和登录界面保持停止，
  下一次真正登录（新 asid）或重启后失效并被删除；`host install` / `host uninstall` 清除它。终端 host 被 stop-host
  停掉时也会写，系统 agent 不接手；Ctrl+C 不写，系统 agent 接手。授权后重启 agent 的提示改成
  `launchctl kickstart -k`（不再先 stop-host，否则标记会让新进程立刻退出）。
- 日志 subscriber 只 init 一次：`LogSink`（追加 ↔ 轮转可切换的 `MakeWriter`），拿到锁时切轮转，`MeshBusy` 让回锁时
  切回追加。`Mesh::start` 遇到 `DataDirInUse` 现在报 `ar_net::MeshBusy`，系统 agent 当作 Taken 处理（放锁、1 s 后重试），
  作为纵深防御。
- 刻意不做：协调 daemon、notify(3) 推送（轮询已够；以后可换 `com.apple.coregraphics.GUIConsoleSessionChanged`）、
  改 plist（不加 ThrottleInterval）、`AI_REMOTE_KEEP_ROOT=1` 下的 fchown（文档注明这个组合未测）。

**已验证**：`cargo fmt --all`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、
`cargo test --workspace --locked` 在这台 Mac 上通过。单测：`handoff::tests`（快速切换、切回、注销/重启、锁屏、
stop-host、崩溃、终端 host、别的用户、`None` 判定、去抖、日志去重、标记语义、标记读写、paused time 下的 1 s 节奏）、
`tests::a_held_claim_keeps_the_profile_until_dropped`、`platform::macos::tests::the_session_ids_the_stop_marker_uses_are_readable`
（`auditinfo_addr` 48 字节、asid 可读、boot uuid 36 字符；结构布局先用 clang 的 `offsetof` 对过：`ai_asid` 在 36）、
hostw 的 `LogSink` 来回切换。**Windows 没编译**（本机没有 Windows target）：Windows 路径只改了共享签名
（`HostOptions` 多一个字段、`run_claimed` 返回值、`supervise` 少一个参数、日志 init 移到 main），靠 CI。

**未验证（要人在这台 Mac 上做，不能动正在跑的 agent 的会话里做的除外）**：

| 场景 | 怎么看 | 期望 |
|---|---|---|
| 快速用户切换到登录界面 | `pgrep -fl ai-remote-hostw`、`host/host.log` | Aqua 那份 `stepping aside` 以 4 退出；LoginWindow 那份 `taking over`；控制端连上看到登录界面。记下交接秒数（推导约 2-3 s + Mesh 启动） |
| 从登录界面切回原会话 | `launchctl print login/<asid>`、日志 | LoginWindow 那份被 launchd 终止（143）或让位（4）；Aqua 那份 2 s 内接手 |
| LoginWindow 那份降权后的 `kCGSSessionOnConsoleKey` | 日志里 `waiting`/`taking over` 行、doctor | 记下实际值（`None` 还是 true/false）；若切回后它仍读到 true/None 且没被终止，Aqua 那份会一直 `waiting: another host` |
| 注销后再登录 / 重启 | 日志 | 新登录界面那份直接接手；登录后 Aqua 那份可能先 `waiting: another host`，再接手 |
| 锁屏 | 日志 | 无交接行 |
| stop-host 之后快速切换、锁屏 | `host status`、日志 | 都保持停止（`not starting: … stop-host`）；下次登录后恢复 |
| 终端 host 运行 10 分钟 | 日志 | 只有一行 `waiting: another host…`，不刷屏 |

**复核后的修正（2026-10-21，同分支，未提交）**：

- LoginWindow 那份读不到会话字典（`None`）时不再无条件算"在控制台上"：改看 `/dev/console` 属主
  （`ConsoleRead { session, device_owner }`，`platform::macos::console_device_owner`）。root → 在；非 root
  （用户会话占屏，切回或别的用户）→ 不在，持锁时 2 次读数后让位；两者都读不到才保留旧的猜测。Aqua 那份仍要
  明确的 true。每份在原始读数变化时记一行 `console read`（真机上据此确认降权后的实际读数）。
- `MeshBusy`：mcpmesh 把 peer store 的任何 `redb::DatabaseError` 都归成 `DataDirInUse`（权限、损坏也算），
  所以系统 agent 只重试 15 s（`MESH_BUSY_LIMIT`），之后以 1 退出交给 launchd；"waiting" 一次只记一行，真正等待过
  一次（`Decision::Wait`）后计时重置。`MeshBusy` 的文案改成"打不开：仍被占用或不可读"。
- stop 标记：只删对所有人都失效的（别的开机、读不出），同一次开机里自己不认的不删（可能是给另一份的）。
  标记加 `login_window`（停止发生在 LoginWindow 那份）：后台等待的 Aqua 那份也认它，直到重启。只有系统 agent
  （`login_window_agent()`）写标记：终端 host / 用户级 LaunchAgent 被 stop-host 停掉不再压住系统 agent。
  系统 agent 写标记失败时拒绝 `Shutdown`，不再悄悄交接。
- 别的用户的会话：在 `adopt_owner` 之后、打开配置目录之前就以 0 退出（以前会因为进不去所有者的 `~/Library`
  以 1 退出、被每 10 s 拉起）；`adopt_owner` 也挪到打开配置目录之前。
- SIGTERM：信号任务循环接收（第二次 SIGTERM 在 host 还在关闭时直接退出）；等待循环开头和 `MeshBusy` 分支检查
  `terminated`，以 143 退出，不再丢信号。
- 仍未做：等待方主动请求持有方让位（控制套接字或 want 文件）。如果真机上 LoginWindow 那份降权后读到的是
  确定但错误的 `Some(true)`，Aqua 那份仍会一直 `waiting: another host`；上表第三行要据 `console read` 日志确认。

## iPad 12.9 英寸优先适配（2026-10-24，`feat/ipad-12-9`）

- `PhoneAdaptiveLayout` 按窗口可用点数和 size class 决策，不按设备型号：regular 且宽 ≥900 时远程画面旁显示 360–440 点智能体面板；
  展开智能体后，宽 ≥800 显示 260–300 点会话侧栏。12.9 英寸横屏 1366 点、竖屏 1024 点均满足；窄分屏回单栏 / sheet。
  正文和输入区最大 820 点，电脑列表宽屏双列；iPad 专属 Info.plist 项启用四向旋转，仍允许系统多任务。
- 旋转、全屏、sheet / 并排 / 整窗切换共用同一个 `PhoneChatStore`，不因布局变化关闭远程订阅。文本和附件草稿按会话暂存；
  直接从侧栏切换时恢复各自草稿，并用选择代数 / 连接代数挡住旧 close、attach 的迟到结果。图片预览同时按宽高 fit。
- Debug 聊天 fixture 可切 Claude / Codex；`-ui-viewport-chat` 在静态远程画面旁打开同一套聊天 fixture；
  `-ui-viewport-resize` 让竖屏容器约束到 683 点、横屏回全宽，专测 sheet ↔ 并排切换（不是系统 Split View 实测）。
- 本机实测：布局决策 12 场景、聊天模型 79 项、viewport geometry、前端 267 项（266 过 / 1 跳过）、TypeScript、IPA 打包 9 项和隐私 7 项通过。
  mac-core 刷新时 `~/.cargo` 符号链接使 Askama 生成路径错误，使用真实 `CARGO_HOME=/Volumes/SSD-2TB/MacData/Development/cargo` 后解决；
  CLT 6.4 缺失 SwiftUI State 宏插件，原始 mac-core 全模块检查仍受既有 `PhoneNetworkSheet` / `PhonePairing` 的 `@State` 阻断。
  仅在 target 中复制源码并将这两处既有 @State 包装换成项目的 PhoneViewState 别名后，全模块 macOS typecheck 通过；没有将该兼容变更混入产品。
- CI 固定 iOS 26.2 的 `iPad Pro (12.9-inch) (6th generation)`（不存在就创建同型号，缺 runtime / device type 明确失败），
  保留 iPhone 回归，新增 5 项 iPad UI 用例，保存 xcresult 和 20 张阶段截图。首轮代码 `7f993b8` 的 CI 39984998117：iPhone 33 项（1 跳过）无失败；iPad 1 过 / 4 失败。
  三项失败的原始 AX 树显示外层面板 ID 覆盖了输入框等子项 ID，草稿值仍在；给面板 / 整窗 / sheet 增加 contain 容器隔离标记。另一项使用了 x=0 的 ScrollView AX 容器边界，改用实际 UITextView 全球坐标检验并排。
  原始日志、6 张已到达截图和 xcresult 保留；下一轮不重跑已通过的 iPhone UI，只跑集成 main 后的完整 iPad 五项。截图改采 XCUIScreen 并附窗口 / 图像方向元数据，不把首轮带黑边的画布当实际方向证明。物理 iPad、
  实际系统 Split View / Stage Manager、外接键盘和 VoiceOver 手动遍历未测，不能用 fixture 截图替代。
- 同步最新 main `028ccc9` 时仅 AGENTS.md 章节插入冲突，保留双方完整记录；在独立 managed worktree 处理，主工作区其他桌面 UI 改动未动。
- Rust fmt / Clippy 通过；全量默认并发首轮在 `ar-net::ipc` 的旧 socket 释放后重绑定用例遇到 AddrInUse（保留失败日志），
  相同代码串行全量重跑 438 过 / 9 忽略 / 0 失败；没有为这次 iPad 布局改动修改 Rust。
- 第二轮 `91ab035` / CI 39992042339：iPad 3 过 / 2 失败，展开重开、预览和侧栏草稿均通过，截图改采 XCUIScreen 后横屏画布正常。剩余一项在键盘弹出后竖屏约束未触发，fixture 改按实际窗口宽度的缩小 / 放大切换 683 点，不依赖 UIDevice 通知；一项是面板 AX 组包围框 y=-9.5，改验实际输入框和导航控件的严格窗口边界。
  截图另证实嵌套 NavigationStack 合并工具栏；常规布局改为 HStack 中两个兄弟导航容器，左侧 Root 工具栏与右侧聊天各自归属，面板显示时隐藏重复打开入口。
- 同步 main 后在隔离检出复跑：默认并发 Rust 全量 439 过 / 9 忽略 / 0 失败、fmt / Clippy / TypeScript 通过；指定 16 文件前端共 362 项，361 过 / 1 跳过，布局 12 场景、聊天模型、viewport、打包和隐私复查通过。
  iOS workflow 默认继续两端 UI 回归；手动 `ipad_only` 只跳过已独立验过的 iPhone UI 套件，仍完整构建设备包和 iPad 验收。

- 再同步 main 的桌面导航 / Claude 细节与诊断记录（1aefe9f），移动端 apps/ios、ar-mobile / controller / net / core、Cargo 与 vendor 相对 519eee1 均无变化；iPad 记录改放文末，避免并行工作反复争用文首插入点。
- 本次 main 同步复查：Rust fmt / Clippy / TypeScript 通过；Rust 全量 439 过 / 9 忽略 / 0 失败，前端 375 项中 374 过 / 1 跳过。移动端源码与 519eee1 一致，当前原生复验沿用该源码。
- 第三轮 519eee1 / CI 39998306949 的五项 iPad 原生验收通过，20 张截图齐全；业务链路、窄窗往返、四向全屏和同会话草稿均完成。逐图复核又发现浅色外观下非全屏左侧导航栏文字在远程黑底不可读，显式设置 canvas 导航栏背景（不动舞台 / 全屏 / 右侧），复验标题与时间可见性。

## 桌面端毛玻璃第二阶段 · 浮层（2026-10-23，`feat/desktop-glass-pages`）

本包只交付浮层；主页面、设置内容、会话外观和全页巡检分后续包。按交接稿的 final-decisions、A D.6/D.7 和
`tests.json` mustKeep 实现；第一阶段之后 main 已合入智能体工作区及 Claude / Codex 视图，验收数量以当前实际结果为准。

- 新 `ui/Modal.tsx`：sheet 与遮罩 portal 到 body，保留 `.cdlg` / `.smoke` / `.nt-pair` 和语义角色；挂载期间
  `html[data-modal]` 是非零计数，后台 `#root` 与较低的 sheet inert。初始焦点、Tab 循环、关闭恢复焦点统一处理；
  控件在异步操作中禁用或被替换时，MutationObserver 把失落到 body 的焦点移回 sheet。
- Confirm、EnableDialog、PairCodeDialog 复用 Modal。安装进行中仍吞掉 Esc / 遮罩点击；配对码关闭仍撤码并解除窗口保护。
  配对框先聚焦始终可用的取消按钮（原先先聚焦“换一个”，随后生成码禁用它，焦点会掉到 body）。
- 决策卡 Esc 只拒绝焦点所在的一张，焦点不在卡里时只拒绝第一张；备注框 Esc 只关闭备注。对话框、菜单、远程输入与输入法组字
  不触发拒绝，忽略长按产生的 repeat。
- `glass/overlays.css`：确认 / 开启 / 配对框宽 440 / 560 / 480 px，thick 玻璃、无动画的祖先、独立遮罩；mac 按钮靠右按
  “取消、主操作”显示，Windows 保留 DOM 顺序。配对二维码缩到 160 px、数字 40 px，避免原 48 px 数字吃掉右内边距。
  泛菜单按平台排版；sheet 是唯一带模糊的入场动画，叠层和模态期间关闭其它菜单动画，减少动态效果时不播放。
- 补回两处 main 集成回归，均由本包的必跑验收发现：Claude 工具行和轮末遗漏文件预览（复用已有 PreviewProvider / PreviewButton /
  TurnFiles，不改文件读取与脚本隔离策略），预览 e2e 的旧选择器同步成 Claude 视图；工作区切换器改用 group + aria-pressed 按钮，
  使网络页文档里恢复只有两枚 radio，切换功能与键盘事件保持原样。
- 新 `tools/desktop-overlays-smoke.py`：headless Camofox REST，真实页面打开三种 sheet；多卡和叠层使用内存 fixture，
  测试专属请求的 deny spy 不发送真实操作。截图只能证明布局，不能证明原生材质。

实测（本机 macOS 27.0.1 / Apple Silicon / 16 GB，Rust 1.95.0、Node 25.5.0、Python 3.14.2）：
- `tsc --noEmit`、`npm run build`、CLI 与 Tauri debug 构建通过；`cargo fmt --all --check`、Clippy `-D warnings` 通过。
- `node --test tools/*.test.mjs` 258 项：257 过、0 失败、1 跳过（PowerShell 原生命令参数测试）；最终代码又跑 1 次同样通过。
- Camofox headless、humanize=true、1280×800、Vite demo：浮层专项最终完整 **111/111 过，1 次，60.36 秒**；网络 **17/17 过，1 次**。
  覆盖三种 sheet 的深浅布局、Tab / Esc、外壳导航隔离、异步禁用焦点、两层 sheet、减少透明度与 demo 撤码。
  多决策卡用真实 store 内存事件与仅 fixture ID 的 deny spy；原生 Tab/Esc，外壳快捷键使用 DOM 事件。截图不证明原生模糊。
- 真实 App + 隔离 quiet host + 替身：预览 **31/31 过，1 次**。首轮完整对话在并行编译、较高换页压力及锁屏期间变慢，
  截图等待超时，第二次 SLOW 未进入运行，手动中止；不计为验收通过，保留诊断日志。原测试此处先等待 running 再直接点 stop，
  超时后表现为 null.click；未找到与本包浮层代码的直接关联，仍需空载复跑，不能把原因直接定为换页压力。
- 原生窗口验收使用的隔离 profile 一开始路径过长，触发 Unix socket `SUN_LEN` 限制；改用短 `/tmp` profile。
  已观测 native / thick 36px 计算样式与 portal / modal=1 / inert，但系统检测本机锁屏且无法自动解锁，未取得可接受的整屏裁窗效果图。

- Rust workspace 全量在 `-j 2 -- --test-threads=1` 条件下通过：437 过、0 失败、9 忽略（1 次）。默认并发首轮在
  `ipc::tests::a_second_daemon_cannot_replace_the_live_socket` 的退出后重绑阶段遇到 AddrInUse，串行复跑通过；未改 Rust。

2026-10-23 当时待补：完整对话 3 次、预览第 2 次、render / offline、真实窗口深浅玻璃截图与原生快捷键；当前收尾见下节与 PR #44。
未测：Windows Mica / 标题条、不同壁纸和失焦对比度、视频帧率对照、真实主机安装及配对。后两项未因本包主动执行。


### PR #44 同步与冲突取舍（2026-10-24）

- 先按用户要求合入 #46（main `27944dd`），再将最新 main 合进本分支，保留实时完整 Workspace、批量会话操作与 iPad 适配。
- Claude 文件预览已在后续 main 中补齐，保留 main 的 grid、ANSI 和 previewedPath 实现，不退回旧包装结构。
  决策卡同时保留 main 的 escapeScope 与本分支的一次一张 / modal / 菜单 / IME 防护，避免聊天 Esc 误拒批准。
- Confirm 的异步动作现在禁用取消与 Esc / 遮罩关闭，失败后留框显示错误并恢复按钮；Camofox 补上 promise 拒绝与作用域双卡检查。
- 本次集成本机静态检查：Rust fmt / Clippy / 默认并发全量 440 过 / 9 忽略，TypeScript、前端 build、前端 422 过 / 1 跳过；
  Camofox 浮层 122 / 122、网络 17 / 17 通过。旧 Node fixture 缺 Element 与真实 card ref，补齐 DOM fixture 后保留原行为断言并通过。
  网络首轮回环请求经系统代理得到 HTTP 400，仅测试进程使用直接回环 opener 后通过，未改系统代理。
- 原生对话 / 预览 / render / offline 回归、深浅色 WindowServer 截图与真实快捷键的最终结果集中记录在 PR #44；
  本机当前未锁屏且录屏权限已授予。证据目录 `/Volumes/SSD-2TB/MacData/Artifacts/ai-remote-pr44-merge-20261024`；旧失败不删除。
