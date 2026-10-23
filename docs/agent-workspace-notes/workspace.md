# 工作区（流名 workspace）开发笔记

设计：`docs/agent-workspace.zh-CN.md` 第 2.1、2.2 节。规格：仓库外 `~/ar-refs/spec/claude-{SB,NS,SP,VS}.md`（洁净室，只按行为和参数实现）。

## 分支与起点

- 第一阶段（前一位同事）：分支 `ws/workspace`，`0bc1d81..aa8d736` 共 7 个提交：多电脑会话列表与合并、左栏、⌘K 搜索面板、
  新建会话页、会话页、演示数据、「电脑 / 智能体」切换与 `?view=agents`、`tools/workspace.test.mjs`。
- 第二阶段（本笔记）：分支 `ws/workspace-s1`，从 `aa8d736` 起，接着做，没有重写已有部分。

## 第二阶段做了什么

| 规格条目 | 做了什么 | 代码 |
|---|---|---|
| SB-09..14、SB-46 | 左栏「会话」标题行右侧加「分组与排序」按钮：分组（文件夹 / 日期 / 状态 / 不分组）、排序（最后活动 / 开始时间 / 名称）、降序 / 升序、显示（未归档 / 已归档 / 全部），恢复默认（已是默认时禁用并写原因）；选择项不关菜单；设置存 localStorage `ai-remote.ws.list` | `lib/workspace.ts`（纯函数）、`ui/workspace/Sidebar.tsx` |
| SB-13 | 按日期分组：今天、昨天、再往前 5 天各一桶（标签按本地语言「10月21日 / Oct 21」）、更早；按开始时间排序时按开始时间归桶；升序时桶整体反转 | `dateBuckets` |
| SB-14 | 按状态分组：需要你（含失败）→ 进行中 → 待查看 → 已完成（已归档或已结束）→ 空闲 | `stateBucket` / `stateBuckets` |
| SB-19 | 不分组时 20 行后才「再显示 N 个」，分组时 5 行 | `rowsBeforeMore` |
| SB-11 / SB-20 | 筛选中按钮变蓝、无障碍标签改为「分组与排序（筛选中）」；筛完为空时居中「没有符合筛选条件的会话」+「显示全部会话」 | `Sidebar.tsx` |
| 设计 1.1 | 只有列表要显示已归档时才带 `archived: true` 调 `chat_sessions` 并保留归档会话；否则行为不变 | `WorkspaceStore.setArchived` |
| SB-26 | 失败会话的菜单把「标记已读 / 未读」换成「标记为完成」（同用字母 u），记下当时的事件号：红点消失、算已读，之后再有事件（再失败）重新显示；toast 可撤销（连已读状态一起恢复）；新建页的「出错」下一步也跟着去掉 | `WorkspaceStore.markDone`、`isAcked`、`actions.ts` |
| SB-47 | 不按文件夹分组时，其他电脑的会话行尾没有状态点就显示电脑图标 | `Row` 的 `env` |
| SB-18 / SB-45 | Alt+点击组头：折叠其他组、展开本组；组头右键菜单：在此新建会话（电脑离线时禁用）、复制路径、折叠其他分组、展开全部分组 | `Section`、`groupMenuItems` |
| SB-39 | ⌘⌥P 置顶、⌘⌥R 重命名、⌘⌥U 已读 / 未读（失败时为标记完成）、⌘⌥A 归档（已归档时取消归档）；作用于左栏里聚焦的行，否则当前会话；按 `e.code` 匹配（Mac 上 Alt 会改 `e.key`）。⌘⇧N 在当前会话所在的电脑和文件夹新建 | `Workspace.tsx` |
| SB-32 | 悬停摘要卡：行上停 500 ms 后在右侧弹 240 px 卡（垂直居中于行、夹在窗口内）：完整标题、文件夹 · 电脑 · 分支、在等什么（琥珀）/ 正在处理 / 出错及原因（红）/ 已结束、最后活动（相对时间）；已有卡时切到下一行立即换；离开 200 ms 关；列表滚动、菜单打开、改名时不弹；触屏不弹 | `RowCard` |
| SB-03 | 窄窗贴边窥视（规格建议的简化路径）：侧栏折叠时指针距左缘 ≤48 px 显示 3 px 提示条，≤16 px 停 100 ms 展开浮层（按着键拖动时不触发）；离开浮层 180 ms 收起，菜单开着或改名框有焦点时每 180 ms 再判断；这样打开时不加遮罩 | `Workspace.tsx` |
| NS-03 | 起始卡不再靠 flex 居中：间隔高度 = max(0, (从容器顶到「输入框底边上移 2.75rem」的高度 − 名义高度) ÷ 2)，名义高度 = 环境卡高 + 2px + 3.25rem（无环境卡 2.5rem）；ResizeObserver 重算。整张「下一步」卡放不下时只保留阻止发送的「选择文件夹」，没有就整张隐藏；记住完整高度防止来回闪 | `NewSession.tsx` |
| NS-19 | ⌘⌥1–9（其他平台 Alt+1–9，排除 AltGr）按环境菜单顺序选电脑，⌘⌥0 配对新电脑；按物理键、输入法合成中不响应、按住重复不执行；菜单项旁显示数字，菜单开着时直接按数字也能选 | `NewSession.tsx` |
| 参考截图 | 「新建」导航行图标换成加号 | `Sidebar.tsx` |

菜单动作（置顶、已读、标记完成、归档、取消归档）抽到 `ui/workspace/actions.ts`，左栏菜单、会话页菜单、快捷键共用。

## 会话页主区：集成时要换的一行

`ClaudeConversation`（`ui/claude/ClaudeConversation.tsx`）在本分支还不存在，非 Codex 会话暂用 `ui/AgentChat.tsx` 的 `Conversation`。
集成时改 `apps/desktop/src/ui/workspace/SessionPage.tsx` 第 115 行：

```tsx
const View = codex ? CodexConversation : Conversation;
```

换成 `ClaudeConversation`（并传 `variant="page"`），删掉对 `Conversation` 的 import。

## 实测（本机 macOS，camofox + vite 演示后端，2026-10-23）

- 端口：任务指定 1431，但已被另一个 worktree（`wf_4368aeee-18a-2`）的 vite 占用，改用 **1441**。
- 截图 19 张，存 `/tmp/ws-workspace-shots/`，文件名前缀 `s1-`（目录里不带前缀的是第一阶段的旧图）：
  深色 / 浅色 × 中 / 英 各有新建会话页、会话页 + 右键菜单（行菜单 / 组头菜单 / 分组与排序菜单 / 已归档行菜单）、搜索面板、窄窗
  （提示条、贴边窥视、窥视 + 悬停卡、窥视 + 行菜单、窥视 + 窄高度只剩「选择文件夹」）；另有按日期分组、按名称不分组、筛选为空。
  每张都看过；四种状态点（灰闪运行中、琥珀需要你、红失败、蓝未读）在新建页截图里同时出现。
- 交互验证（在页面里派发事件后读 DOM，单次）：
  - 「状态」分组 + 「全部会话」：已归档的「Spike: GraphQL gateway」出现在「已完成」桶，标题变灰；取消归档后 toast「Unarchived …」，
    「已归档」视图变空并显示空态。
  - 失败行「标记为完成」：红点消失、toast 带撤销；`localStorage['ai-remote.ws.acked']` 记下事件号。
  - 窄窗（iframe 680×560）：指针移到 x=5 → 100 ms 后 `data-side=float`；派发 `pointerleave` → 300 ms 内回到 `hidden`。
  - 窄高度（iframe 高 400）：「下一步」只剩「选择文件夹」；高度改回 700 后三行恢复。
  - 悬停卡：派发 `pointerover` 后 700 ms 出现，内容与行一致。
- 门槛（全部通过）：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、
  `cargo test --workspace --locked`（本阶段没改 Rust，照跑一次）、`node --test tools/*.test.mjs`（162 项，160 过，2 跳过，0 失败）、
  `cd apps/desktop && npx tsc --noEmit -p .`。

## 踩过的坑

- **类名撞车**：悬停卡起名 `.ws-card`，和新建页起始卡的 `.ws-card` 撞了（新建页卡片被改成 fixed 240 px）。改名 `.ws-hovercard`。
  新类名先 `grep -rhoE '\.(类名)\b' styles` 看有没有已有定义。
- **同一 tick 连点两项**：「分组与排序」最初用 `setPrefs({...prefs, x})`，同一 tick 两次选择时后一次覆盖前一次。改成函数式更新。
- **「下一步」放不下的判定**：只用一行步骤的高度判断，三行的卡仍会被挤进滚动区；用整张卡的高度判断又会在隐藏后变「放得下」来回切。
  做法是不在收起状态时记下整张卡的完整高度，收起期间用记下的值判断。
- **合成事件测菜单**：camofox 派发的 `contextmenu` 前面没有 `pointerdown`，前一个菜单不会自动关；真实右键没有这个问题。
- 截图里 iframe 测窄窗：在同源页面里把 body 换成 iframe；中途改 localStorage 后外层页面偶尔会整页刷新，重新换一次即可。

## 和设计 / 规格不一致之处

- 「分组与排序」按钮放在新增的「会话」标题行上（规格 SB-12 只说按钮标签，没说平铺变体里的位置）；参考截图里没有这一行。
- 菜单弹层左对齐按钮左缘，规格 SB-11 是右对齐（align=end）；`PopMenu` 只支持左对齐，没改。
- 「标记为完成」只对失败会话提供；规格里它也能解除「需要你」，但本项目的「需要你」来自实时的待审批 / 提问（`pending`），
  把它藏起来会掩盖真的在等人的会话，所以没做。
- 状态分组里「已完成」= 已归档或已结束（规格还包括 PR 合并 / 关闭，本项目没有 PR 数据）。
- 会话行尾仍显示分支（设计 2.2 写了「行尾分支」；规格 SB-21 行尾是 PR 图标，分支在元数据第二行）。
- 悬停卡不能作答（规格里提问和改动预览可在卡里回答），只显示。
- 贴边窥视只做了规格建议的简化路径（48 / 16 / 100 / 180），默认路径的算法规格里拿不到。

## 没做 / 没测

- 没做：数字跳转（SB-41，按住 Ctrl+Shift 200 ms 显示 1–9）、多选与批量菜单（SB-34）、拖拽置顶 / 排序 / 进组（SB-42）、
  自建分组与「移动到组」（SB-08 / SB-36）、组顺序自定义（SB-15）、元数据第二行开关（SB-28）、子会话树（SB-29，被控端没有父子关系）、
  「更早」桶的批量归档 / 删除（SB-38）、搜索面板的「曾用名」和深入搜索（SB-06）、展开收起动画（VS-34）、密度开关（SB-04）、
  新建页环境栏芯片在窄宽度下缩成图标（NS-34）、发送后状态行与排队（NS-36）、「存为环境」提示（NS-42）。
- 没测：真实 Tauri 窗口（只在浏览器演示后端里看）、真实被控端的 `chat_sessions archived:true`（演示后端已实现同样参数）、
  Windows 上 Alt+数字与 AltGr、触屏、读屏器。
- `ClaudeConversation` 尚未合入，会话页非 Codex 会话的主区仍是侧栏对话组件。

## 新增测试与命令

- `tools/workspace.test.mjs` 新增 6 项：列表设置的默认值与升降序、状态筛选、按日期分桶（含按开始时间、升序反转）、按状态分桶顺序、
  「标记为完成」及撤销与再失败、只在需要时请求并保留已归档会话。
- 命令：`node --test tools/workspace.test.mjs`（32 项全过）；全部前端测试 `node --test tools/*.test.mjs`。

## 端到端：`tools/desktop-workspace-e2e.js`（2026-10-23）

真实调试版桌面包 + 本机隔离被控端（两端 quiet）+ 替身智能体，经 `desktop-chat-e2e.py` 驱动，从电脑页开始：

```sh
E2E_SCRIPT=tools/desktop-workspace-e2e.js E2E_QUERY='view=devices&shotlang=zh&shottheme=dark' \
  python3 tools/desktop-chat-e2e.py --secs 600 --out /tmp/ws-e2e-zh-dark      # 另一种外观：shotlang=en&shottheme=light
```

- 覆盖 102 项：A 工作区（电脑 / 智能体切换、新建页选电脑 / 浏览文件夹 / 权限模式、分组与标题兜底→替换、运行 / 未读 / 失败 /
  待回答四种点、标记完成、置顶、F2 与菜单改名并经 `chat_sessions` 核对被控端、归档与取消归档、⌘K 按标题与文件夹、↑↓、⌘B、
  边缘拖宽与双击复位、Codex 会话进 Codex 视图）；B Claude 页（询问模式的审批卡：停靠、可打字、400 ms 内 ⌘↵ 不生效、允许一次 /
  本会话允许 / 拒绝；两题提问含多选与「其他」；计划卡退回附意见、批准并切到自动接受编辑后模式芯片跟随被控端；模式菜单切到计划；
  diff 真实行号；停止后继续；结束会话；排队消息编辑与撤回）；C 第二条连接（页面内另开 `open_chat`）先批准，桌面端卡片收起。
- 替身新增：LATER<n>（n 秒后自己结束）、FAIL、EDITAT（22 / 40 行两段 hunks）、DELAYTITLE（标题晚 4 s）、每次提问 id 不同。
  `E2E_QUERY` 换起始页面。脚本开头去掉共享存储里旧 e2e 主机的残留，结束时还原它动过的 `ai-remote.mode` / `ai-remote.ws.*`。
- 实测（本机 debug 包，各 1 次）：深色中文 102 项 96 过，浅色英文 102 项 96 过；6 项失败都是下面的产品问题。
  侧栏状态点来自轮询：琥珀点出现 / 消失比卡片晚 0–20 s（屏幕外窗口，计时器可能被节流；脚本等 30 s 并记下实际用时）。
- 发现的问题：
  - 归档视图 / 全部会话：被控端 `chat_sessions archived:true` 只回已归档的（`crates/ar-host/src/tools/chat.rs:83`），前端当作
    「连同已归档的」（`apps/desktop/src/lib/workspace.ts:771`，演示后端也是这么实现的）。切到「已归档的」时正打开的会话页变成
    「找不到这个会话」，「全部会话」只列已归档的。
  - 标记为完成后改名，红点又回来：改名产生 title 事件，`last_seq` 变大，`isAcked` 不再成立（`lib/workspace.ts:196`）。
  - 审批卡不能附说明拒绝（`ui/claude/Cards.tsx:154` 起只有拒绝 / 本会话允许 / 允许一次；被控端支持 `permit.deny.message`）。
  - 以询问模式新建的会话，输入框下的模式芯片显示「权限模式 ?」直到第一次切换：被控端初始模式与元数据相同就不发 mode 事件
    （`crates/ar-host/src/chat/mod.rs:1711`），芯片只认事件（`ui/claude/ClaudeConversation.tsx` 的 `ModePicker mode={chat.mode}`）。
  - 一次编辑显示两份 diff（输入里的旧 / 新文本一份、结果的 hunks 一份），标题上的 +/− 也加了两遍：`tool_update` 把内容追加在
    `tool_call` 的内容后面（`apps/desktop/src/lib/chat.ts:473`）。原有的 `desktop-chat-e2e.js` 也因此失一项
    （the group sums up the edit：+4−2；改动前后的 harness 各跑 1 次结果相同，67 项里只此一项失败）。
  - 另一端先批准：卡片收起、工具照常运行，但页面上看不出是别的设备处理的（`resolved.by` 收到了没显示）。
  - 小问题：提示条压在会话页输入框上；新建页「最近位置」会列出已不在电脑列表里的电脑（旧 e2e 主机）；`eventPatch`
    （`lib/workspace.ts:109`）没有调用方，打开的会话的状态点也只能等轮询。
- 坑：同 bundle id 的另一个调试版桌面包同时开着时，两者共用 WebKit 存储，曾把本次运行的页面切走、改写 `ws.side`；
  合成指针事件对 `setPointerCapture` 不是有效指针，拖边时临时替换了它；菜单在窗口失焦时会关，脚本打开菜单会重试。


## 阶段追记：SB-34 多选与批量操作（2026-10-23，P3）

本节是后续实现记录；上文的历史“没做 / 没测”和测试数字保留为对应阶段的快照。

- **已实现的范围**：Cmd/Ctrl 点击切换多选、Shift 可见行范围、选中高亮，以及选中行右键的批量置顶/取消置顶、已读/未读、归档/取消归档。
  Esc、普通打开会话或无修饰点击行外（含列表空白和组头）清空选择；选择不持久化。全选至少两个会话，有折叠分区时提示先展开。
  正在运行或待处理的会话不能批量改已读；默认文件夹与电脑分组、数字跳转保持原有行为。
- **执行与撤销**：远端批次共享最多 8 个并发，同会话去重并串行编辑；逐项收集成功/失败，提示部分成功和具体失败原因。
  撤销只处理成功且仍符合本次结果的项，恢复本次涉及的 pinned/archived 字段，保留期间收到的标题等更新；本地已读一次更新快照并按既有键持久化。
  没有新增删除、模型或工作目录修改操作。
- **本阶段实测**：合入 `028ccc9` 完全离屏验收基线后，本树 `node --test tools/*.test.mjs` 共 391 项（390 过、1 个平台项跳过），
  TypeScript 检查通过；前述 Camofox 演示 24 项仍只作为浏览器流程证据，截图存在已记录的字体回退条件。
- **原生仍未测**：P3 专项 E2E 已追加 UI 多选与 P/U/A、真实被控端持久化查询及撤销断言，另有批量截图前的 WK FontFace 加载断言；
  本次只做源码复核、脚本语法及上述前端门槛，没有启动 Cargo、原生应用、被控端或浏览器。P1 的原生通过结果不替代这批新增操作的原生验收。
  字体断言在共用 5 秒等待预算内依次等待 fonts.ready/load，要求对应 family 非空且全部 loaded；预算不是实测耗时，也不证明每个字形的排他字体选择。


## 阶段追记：Windows 原生聊天验收入口（2026-10-24，P3）

新增独立 Windows 2025 PR/手动 CI，固定工具版本并复用 restore-only 依赖缓存；既有 required jobs 不变。
CLI 与 Tauri debug/no-bundle 构建后，六套替身各两遍，逐套保存真实 harness/App 退出码、精确检查计数、隔离与超时状态；失败不会靠后续通过覆盖。
公开 artifact 只含摘要和按真实 DOM/客户区几何裁出的测试 UI，原始屏幕、日志及认证/临时 home 不上传。
不可靠裁框明确未上传，截图有效性不替代后续逐图 QA。该入口目前仅有本地便携测试与语法检查，Windows 云端尚未运行，历史测试数字保留。
