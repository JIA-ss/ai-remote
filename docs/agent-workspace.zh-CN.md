# 智能体工作区：设计

目标：桌面端新增「智能体」工作区，交互上和官方 Claude 桌面 app 的代码模式（Code 标签页）一致：左栏按项目列出各台被控端上的会话，
主区是整页对话或新建会话页。Claude（以及 ACP 智能体）会话按 Claude app 的表现做，Codex 会话按 Codex app 的表现做
（Codex 页面由 `feat/codex-chat-view` 提供，入口组件 `CodexConversation`）。实时电脑画面在左，完整工作区在右，CLI、工程、会话管理及对话直接复用，不再另设推流页的选择表单。

做法是洁净室：官方 app 的行为先整理成不含代码的规格（在仓库外），实现只依据规格和本文，不照搬官方代码、字体和文案。

## 1. 被控端协议补充（向后兼容）

所有新字段都是可选的（`skip_serializing_if`），旧控制端忽略；新控制端遇到旧被控端时按"没有这项能力"处理。

### 1.1 会话元数据（`<id>/meta.json`，`chat_sessions` 原样返回）

| 字段 | 类型 | 含义 |
|---|---|---|
| `title` | string? | 会话标题。没有时界面用首条消息截断 |
| `title_source` | `"auto"`\|`"agent"`\|`"user"`? | 标题来源；`user` 改过的不再被自动标题覆盖 |
| `first_text` | string? | 首条消息前 200 字符（列表兜底标题、搜索用） |
| `last_status` | `"completed"`\|`"failed"`\|`"interrupted"`? | 上一轮怎么结束的 |
| `last_reason` | string? | 上一轮失败的原因（截 300 字符） |
| `pending` | `"permission"`\|`"question"`\|`"plan"`? | 有等人回答的审批 / 提问 / 计划；回答或轮次结束后清除 |
| `last_seq` | u64 | 最后一个事件的 seq（控制端据此算未读） |
| `branch` | string? | 工作目录所在 git 仓库的当前分支：直接读 `.git/HEAD`（worktree 的 `.git` 文件指向的 gitdir），**不执行 git**（服务模式下被控端是 SYSTEM，仓库配置能让 git 执行程序） |
| `pinned` | bool | 置顶 |
| `archived` | bool | 归档（列表默认不显示；被控端仍按原规则清理） |
| `permission_mode` | string? | Claude 的权限模式：`default`\|`acceptEdits`\|`plan`\|`bypassPermissions` |
| `parent` | string? | 分叉出来的会话：从哪个会话（同一被控端）分叉（SB-29 子会话树） |
| `fork_from` | string? | 只在被控端内部用：分叉时父会话的 `proto_session`，第一个进程据此复制对话；`chat_sessions` 不返回 |
| `previous_titles` | string[]? | 以前用过的标题，旧的在前，最多 10 个；改名（用户或智能体）时把旧标题推入（SB-06 曾用名） |

`chat_sessions` 增加可选参数 `archived: bool`（默认只返回未归档的；`true` 只返回已归档的）。

实现说明（2026-10-23，`ws/host-s1`）：`last_seq` 不写进 `meta.json`，`chat_sessions` 按转录的 seq 现算（和旧字段 `seq` 相同）；
`branch` 也是每次列出时现读，`HEAD` 游离（detached）时不给；没有值的新字段不出现在 `chat_sessions` 里（和旧被控端一样），
`pinned` / `archived` 总是给。旧 `meta.json` 在被控端启动时补齐：`first_text` 和 `last_status` / `last_reason` 从转录里读，
Claude 会话的 `permission_mode` 记为 `bypassPermissions`（旧会话都是这么跑的），其他智能体没有 `permission_mode`。

新工具 `chat_update`（admin 级，默认允许，`CHANNEL_TOOLS` 里，不对 MCP 客户端列出）：参数 `session` 加任意几项
`title`（空串表示恢复自动标题）、`pinned`、`archived`。改标题时 `title_source = "user"`。返回更新后的 meta。

分叉（2026-10-26）：`chat_start` 新增可选参数 `fork_of`（同一被控端上的会话 id），此时不给 `runtime` / `cwd` / `model` /
`permission_mode`（给了就拒绝），都取父会话；新会话 meta 记 `parent`，没有自己的标题（界面显示「父标题 的分叉」/「Fork of …」）。
转录先复制父会话已结束的轮次（重新编号、去掉 `title` 事件，正在跑的那一轮不复制），第一个进程用父会话的 `proto_session`
分叉：Claude `--resume <id> --fork-session`，Codex `thread/fork`（`excludeTurns: true`，审批与沙箱同 `thread/start`）。
拒绝并写明原因：ACP 智能体、父会话还没有 `proto_session`（没跑过轮次）。父会话运行中也允许。经 runner 时用单独的
`fork_from` 字段，旧 runner 不认识时开新对话，不会续上父会话的对话。

新工具 `chat_search`（admin 级，默认允许，只读，`CHANNEL_TOOLS` 里）：参数 `query`、`limit`（默认 20，最多 50）。
在本机所有会话的 `events.jsonl` 里按"每个词都包含、不区分大小写"搜 `user` / `message` 正文（同一条回答的分段拼起来，
不搜工具输出和思考），每个会话最多 3 段带上下文的摘录（≤160 字符）和命中段数，按最后活动排序；逐行读，
1.5 s / 256 MiB 预算，超了返回 `truncated: true`。

### 1.2 新事件（chat/1 下行）

```json
{"t":"title","title":"修复 build 报错","source":"agent"}
{"t":"permission","rid":"p3","tool":"Bash","kind":"execute","title":"cargo build -p ar-host","input":{...预览...},"reason":"…","suggestions":["session"]}
{"t":"plan_review","rid":"p4","plan":"## 计划\n1. …"}
{"t":"question","qid":"q2","prompt":"用哪个配置？","options":["debug","release"],"questions":[{"header":"配置","question":"用哪个配置？","options":[{"label":"debug","description":"…"}],"multi":false}]}
{"t":"resolved","id":"p3","outcome":"allowed","by":{"device":"…"}}
{"t":"mode","mode":"acceptEdits"}
```

- `permission`：智能体要用一个工具、当前权限模式要求问人。`suggestions` 里有 `session` 表示可以"本会话一直允许"。
  另带可选 `call_id`（对应的 `tool_call`，Claude 的 `tool_use_id`）。
- `plan_review`：Claude 的 ExitPlanMode，展示计划并让人选接下来的权限模式。
- `question` 新增 `questions`（多题、带说明、可多选）；旧字段 `prompt` / `options` 保留（第一题），旧控制端照旧能答第一题。
- `resolved`：审批 / 提问 / 计划被回答（或轮次结束作废）后发出，所有查看者据此收起卡片（先到先得）。
  `outcome`：`allowed` / `allowed_session` / `denied` / `answered` / `cancelled`。
- `mode`：Claude 的权限模式变了（开局、用户切换、计划批准后切换）。
- `ToolContent::Diff` 增加可选 `hunks: [{old_start, old_lines, new_start, new_lines, lines:[" 行","-行","+行"]}]`：
  Claude 来自 `tool_use_result.structuredPatch`，Codex 来自统一 diff 的 `@@` 头。有 `hunks` 时界面显示真实行号。

新通知码进 `NOTICE_WORDS`：`mode_rejected`、`permission_expired`。

### 1.3 新上行命令

```json
{"permit":{"rid":"p3","decision":"once"|"session"|"deny","message":"别删那个目录"}}
{"permit":{"rid":"p4","decision":"once","mode":"acceptEdits"}}          // 批准计划并切到某模式
{"permit":{"rid":"p4","decision":"deny","message":"先别改 CI"}}         // 计划要改
{"answer":{"qid":"q2","text":"release","answers":{"用哪个配置？":"release"}}}
{"mode":{"mode":"plan"}}
```

- `permit` / `answer` 对已经 `resolved` 的 id 回 `{"t":"error","code":"resolved","id":"…"}`（另一台设备先答了，或轮次已结束）。
  先到先得在被控端会话里原子地做（第一个回答把它从"等人回答"里取走），只读票据照旧回 `readonly`。
- `mode` 只对 Claude 有效，其他智能体回通知 `mode_rejected`。
- hello 增加 `caps.permissions: true`（支持 permit / mode）、`caps.questions_multi: true`（只有 Claude Code 有；同时 Claude
  的 `caps.questions` 变成 `true`）。`chat_runtimes` 的 caps 同样带这两项。

### 1.4 Claude 适配器

- 启动参数：去掉 `--disallowedTools AskUserQuestion`；加 `--permission-prompt-tool stdio`；`--permission-mode` 用会话的模式
  （新会话默认 `bypassPermissions`，和现在一致；`chat_start` 新增可选参数 `permission_mode`）。
- `can_use_tool`：
  - `AskUserQuestion` → `question`（带 `questions`），回答后回 `allow`，`updatedInput = {questions, answers}`。
  - `ExitPlanMode` → `plan_review`；批准回 `allow` 并发 `set_permission_mode` 到所选模式；要改回 `deny` 带说明。
  - 其他工具：模式是 `bypassPermissions` 时直接 allow（保留现在把 `run_in_background` 改成 false 的处理）；否则发 `permission`，
    `once` → allow，`session` → allow 并带上请求里的 `permission_suggestions` 作为 `updatedPermissions`，**但每条的
    `destination` 改成 `session`**（实测：原样回传时建议里写的是 `localSettings`，Claude Code 会把规则写进项目的
    `.claude/settings.local.json`，下次会话、别人在这个目录里也生效；改成 `session` 后同一会话再跑同一命令不再问，也不写文件），
    `deny` → deny（带 message 时 `interrupt:false`，模型会看到说明；不带时 `interrupt:true`）。
  - 轮次被打断或进程退出时，未答的请求全部 `resolved cancelled`。打断时适配器先对未答的请求回 deny（`interrupt:true`），再发
    `interrupt`，免得 Claude Code 一直等这些回答；Claude Code 发 `control_cancel_request` 收回某个请求时也按 `cancelled` 处理。
  - 不再持有的请求（已被收回）收到 `permit` 时发通知 `permission_expired`。
- `mode` 命令 → `control_request` `set_permission_mode`。模式变化以 Claude Code 自己说的为准：`system` 行（`init`、`status`）
  里的 `permissionMode` 一变就发 `mode`；`set_permission_mode` 失败回通知 `mode_rejected`。会话没有进程时 `mode` 命令直接改
  `meta.permission_mode` 并发 `mode`，下次启动用它。
- 标题：首条消息被接受后发 `control_request` `generate_session_title`（`description` = 首条消息，`persist:false`），
  回应里的 `title` → `title` 事件（`source:"agent"`）；不回或失败就不发（界面用首条消息兜底）。用户用 `chat_update` 改过名后，
  智能体的标题不再覆盖；改名本身也发一条 `title`（`source:"user"`）。
- diff：读 user 消息顶层的 `tool_use_result.structuredPatch`，作为对应 tool_call 的 `tool_update` 里带 `hunks` 的 diff。
  只在该 user 消息恰好有一个 `tool_result` 时读（`tool_use_result` 是消息级字段）；新建文件的 `structuredPatch` 是空数组，不出 diff。

实测（2026-10-23，本机 Claude Code 2.1.308，`--model haiku`，`--permission-prompt-tool stdio`，每项 1 次）：

| 问题 | 结果 |
|---|---|
| bypass 下 AskUserQuestion 还走 `can_use_tool` 吗 | 走。请求带 `tool_use_id`、`requires_user_interaction:true`，input 是 `{questions:[{question,header,options:[{label,description}],multiSelect}]}` |
| 回 allow + `updatedInput {questions, answers}` 后模型拿到答案吗 | 拿到。工具结果是 `The user answered: "<问题>"="blue"…`，模型回 `YOU CHOSE blue` |
| plan 模式下 ExitPlanMode | 走 `can_use_tool`，input 是 `{plan, planFilePath}`（assistant 消息里的 tool_use input 是空的，计划只在这个请求里）；回 allow 后工具结果是 "User has approved your plan" |
| default 模式 Bash | 只读命令（`echo`）不问；`curl …` 问，带 `permission_suggestions:[{type:addRules, rules:[{toolName:"Bash",ruleContent:"<命令>"}], behavior:"allow", destination:"localSettings"}]`、`decision_reason`、`description` |
| default 模式 Edit | 问，`permission_suggestions:[{type:"setMode", mode:"acceptEdits", destination:"session"}]` |
| deny 带 message、`interrupt:false` | 工具结果是 message、`is_error:true`，轮次继续（模型可能不照 message 里的话做，只当作说明） |
| `set_permission_mode` | 回 `{mode}`，随后一行 `system` `status` 带新 `permissionMode`；不认识的模式回 error `invalid_mode`（Claude 还认 `auto`、`dontAsk`，我们只放四个） |
| `generate_session_title` | 回 `{title}`，约 2–3 s；标题语言跟用户设置走（本机全局设置要求中文，英文描述也得到中文标题） |
| `structuredPatch` | user 消息顶层 `tool_use_result.structuredPatch: [{oldStart, oldLines, newStart, newLines, lines:[" a","-b","+c"]}]`，同级还有 `filePath`、`oldString`、`newString`、`originalFile` |
| 我们写回的 `control_response` | 会被原样回显一次（`--replay-user-messages`），按 `request_id` 不是我们发的请求，忽略即可 |

另用真实 Claude Code 跑过整条会话链（`real_claude_asks_titles_and_switches_modes`，忽略的单测）：标题、提问按题回答、Bash 本会话允许（目录里没写出
`settings.local.json`）、切到 plan 模式，1 次全过。

### 1.5 runner（服务模式）

管道协议同步扩展：`LiveCmd::Permit`、`LiveCmd::SetMode`、`Answer` 带 `answers`；新事件经 `Out::Ev` 原样传。
另加 `LiveCmd::GenerateTitle`（首条消息后要标题），`launch` 请求带 `permission_mode`。

### 1.6 模型与推理强度（2026-10-26）

规格 NS-29 / NS-32 / NS-33 / CP-53 / CP-57。全部是可选字段，旧控制端忽略；旧被控端的 hello 没有 `models`，新控制端据此不显示会话页的模型按钮。

```json
{"t":"models","models":[{"id":"opus","name":"Opus 5.5","description":"…","efforts":["low","medium","high","xhigh","max"],"default":true},
                        {"id":"gpt-x","name":"GPT X","efforts":["low","medium","high"],"default_effort":"medium"}]}
{"t":"model","model":"haiku","effort":"high"}            // null：智能体自己的默认
{"model":{"model":"haiku","effort":"low"}}               // 上行命令；null 或不写 = 默认
```

- **数据来源**：Claude Code 启动时的 `control_request initialize` 应答带 `models`（`value`、`displayName`、`description`、
  `supportsEffort`、`supportedEffortLevels`）；第一项 `value: "default"`（`resolvedModel` 是默认实际用的模型）不列出，改为把
  `resolvedModel` 相同的第一项标 `default`；`supportsEffort` 为 false 或没有档位的模型 `efforts` 为空（例如 Haiku 4.5）。
  Codex 在线程起来后调 `model/list`（`{limit:100}`），取 `id`、`displayName`、`description`、`isDefault`、`defaultReasoningEffort`、
  `supportedReasoningEfforts[].reasoningEffort`，`hidden` 的不列。ACP 智能体取 `model` 配置项的可选值（不分档）。
- **会话中换**：Claude Code 用 `control_request set_model`（`default` 表示默认），应答成功才记下并发 `model`，失败发通知
  `model_rejected`（原话，例如 `Model 'x' not found`）。没有换档位的控制请求（2.1.308 对 `set_effort` 回 "Unsupported control
  request subtype"），所以档位变了时：会话立即记下并发 `model`，在没有轮次进行时停掉进程、以新的 `--model` / `--effort` 和
  `--resume` 重新启动（发 `resumed` 通知）；轮次进行中则等这一轮结束再重启。Codex 不需要重启：`turn/start` 的 `model` /
  `effort` 作用于这一轮及之后，`null` 表示沿用，所以适配器每轮带上选定的值；只选模型时带该模型的 `defaultReasoningEffort`；
  选过之后改回默认，显式发线程原来的模型和档位（`thread/start` / `thread/resume` 应答里的 `model`、`reasoningEffort`，没有时用列表的
  默认项）。第一轮等 `model/list` 应答（一次往返）后再发。ACP 用 `session/set_config_option`。
- **持久化**：`meta.json` 记 `model`、`effort`、`models`；`chat_sessions` 带 `effort`；被控端按智能体 id 记最后一次的列表（启动时从
  各会话的 meta 恢复），`chat_runtimes` 的每个智能体带 `models`（没运行过就没有）。`chat_start` 新增可选 `effort`；校验：模型名
  `valid_token`、档位小写字母数字和 `-_`、最长 16；所选模型（在已知列表里）不接受的档位被去掉。审计 `chat_session` `model`。
- **runner**：`LiveCmd::SetModel` 经管道为 `{"t":"model","model","effort"}`，launch 请求带 `effort`，`Out::ModelSet` 原样回传。
  旧 runner（跨被控端重启活下来的）不认 `model` 命令时这条命令丢失，会话的模型不变（不报错）。
- **桌面端**：新建会话页的模型芯片和会话页输入框下巴右侧的按钮打开同一个菜单（`ui/workspace/ModelMenu.tsx`）：标题「模型」/
  「模型 · 仅本会话」，默认项（有默认模型时写出名字）、各模型（带一行说明）、用过的自定义模型、推理强度一段（只列所选模型接受的
  档位，加「默认」）、最底下「自定义模型 ID…」输入框。芯片是模型名 + 次级色的档位词。⌘⇧I 打开，⌘⇧E 打开并聚焦档位。
  新建会话页按「电脑 + 文件夹 + 智能体」在开会话时记住模型和档位（localStorage `ai-remote.ws.place-models`），没有记录的文件夹
  用它最近一个同智能体会话的值（模型仍在列表里时）；权限模式不记。被控端还没报过列表时用 Claude Code 的别名（Opus / Sonnet /
  Haiku，五档全给）。

## 2. 桌面端结构

规格（仓库外，行为 + 参数 + 依据，不含官方代码）：`~/ar-refs/spec/claude-{SB,NS,SP,TR,TC,CP,VS}.md`
（左栏 / 新建会话页 / 会话页框架 / 转录 / 卡片 / 输入框 / 视觉），截图 `~/ar-refs/spec/claude-new-session-sidebar.png`。
Codex 的规格在同目录 `codex-X*.md`，Codex 页面归 `ui/codex/`。

### 2.1 入口与路由

- 主窗口（玻璃外壳）左栏顶部加两段式切换「电脑 / 智能体」（位置和形态仿规格 SB-01 的对话 / 代码切换；展开时在顶栏右端，
  紧凑 72 px 时竖排在顶栏下方）。选「智能体」进入左侧实时画面与右侧完整工作区的并排视图；工作区顶部原有切换选「电脑」
  回到电脑列表。上次所在的半边记在 localStorage（`ai-remote.mode`），打开 app 且网址没指定页面时回到那半边。
- 工作区是外壳的一个路由：`lib/nav.ts` 的 `Route` 增加 `{ page: "agents"; host?: string; session?: string; place?: Place | null }`
  （`place` 是新建会话页的预填，不算不同位置）；`?view=agents[&host=&session=]` 直接打开（截图、e2e）。工作区里开会话、新建、
  「在推流里打开」、设置、配对都是外壳历史里的普通条目，‹ › / ⌘[ ⌘] / 鼠标侧键跨两半边走同一条历史；会话的逻辑父级是新建会话页。
  窗口藏到托盘时，从工作区打开的推流回到工作区（推流条目丢掉），其余推流照旧回到它的电脑页。工作区里外壳只留历史、设置和页面快捷键，
  ⌘N、侧栏开关、搜索归工作区。
- 实时画面和智能体入口都进入 `LiveWorkspace`：左侧 `SessionPage` 只按电脑更换，右侧完整复用 `Workspace embedded`。同机切会话、工程、新建和历史不重开视频；换电脑时左右同步。CLI、项目和会话管理沿用现有侧栏/新建输入，不另加选择条；右侧按自身容器宽度使用原侧栏或抽屉。
- 窗口隐藏时保留当前工作区选择、停止视频和远程输入，显示后才重新观看；全屏只隐藏右侧，不卸载会话。右侧键盘归属随实际焦点/点击，远程画面按键不被菜单或审批卡截走。

### 2.2 工作区（`ui/workspace/`，`lib/workspace.ts`，`styles/workspace.css`，文案 `ws_`）

- **数据**：`lib/workspace.ts` 的 store 在工作区可见时每 4 s（不可见 20 s）对每台在线电脑调 `chat_sessions`（旧被控端没有新字段时按兜底规则显示），
  合并成 `{host, meta}` 列表；会话打开时也用对话通道的事件即时更新（标题、状态、pending）。未读：本机按 `host/id` 记"看过的 seq"，
  `last_seq` 大于它且不是当前打开的会话即未读。置顶 / 归档 / 改名走 `chat_update`（旧被控端没有时：置顶记本机，归档与改名禁用）。
  分组、排序、搜索、状态点都写成纯函数，`tools/workspace.test.mjs` 覆盖。
- **左栏**（规格 SB）：平铺变体（贴左全高、右侧分隔线）；顶栏 48 px（红绿灯占位、显隐侧栏、后退 / 前进、模式切换）；搜索条（伪输入框，点开 ⌘K 面板，
  按标题 / 文件夹 / 分支 / 电脑名搜，AND 匹配）；导航行「新建」；会话区：置顶分区 + 按「文件夹 · 电脑」分组的项目分区（组头可折叠、`+` 在该项目新建、
  非本机带电脑图标；每组先显示 5 行，"显示更多"）；会话行一行标题、末端渐隐、行尾分支 / 状态点（运行中灰色闪烁、需要你琥珀、未读蓝、失败红），
  悬停换成 `…` 菜单；右键 / `…` 菜单：重命名、置顶、标记已读 / 未读、归档、结束会话；密度紧凑档（行高 28、间距 2）；宽度 200–420、默认 288、
  拖拽和双击复位、⌘B 显隐；键盘 ↑↓ / Enter / F2 重命名。底部一行显示本设备名和设置入口。
- **新建会话页**（规格 NS）：主区中间一张选择卡：最近项目（电脑 + 文件夹）列表，底部「选择环境」下拉（在线电脑）和「浏览文件夹…」（复用
  `FolderPicker`）；下方输入框，输入框下一行芯片：电脑 · 文件夹 · 权限模式（Claude：询问 / 自动接受编辑 / 计划 / 跳过权限确认）· 智能体与模型。
  发首条消息即 `chat_start` + 发送，跳到会话页；标题先用首条消息，被控端回 `title` 后替换。
- **会话页框架**（规格 SP）：顶栏 48 px（标题可点改名、后面灰色「文件夹 · 电脑 · 分支」，右侧：在推流里打开这台电脑、更多菜单：归档、结束），
  主区是对话组件（Codex 会话 → `CodexConversation`，其他 → `ClaudeConversation`），对话内容列最大宽度按规格 TR。
  电脑离线 / 会话已结束 / 被控端太旧时整页状态按规格。

### 2.3 Claude 会话页（`ui/claude/`，`styles/claude.css`，文案 `cl_`）

入口 `ui/claude/ClaudeConversation.tsx` 导出 `ClaudeConversation`，props 与 `CodexConversation` 一致：`{ host, target, onLeave }`，
另加可选 `variant?: "page" | "rail"`（工作区整页 / 推流右栏窄版）。按规格 TR / TC / CP / VS 实现：
- 转录：回合 → 行；连续工具与思考合成一组，运行中或待审批展开、结束后折成"动词 + 计数"一句（最多 3 项、其余"另 N 项"）；用户消息气泡；
  正文最大宽度；回合尾工具栏（复制、耗时）；改动汇总；跳到最新；跟随到底（离底 8 px + 滚动方向）；流式 markdown 只重解析尾块
  （`lib/markdown.ts` 增量接口，旧接口保留）；按可见性节流（可见 33 ms / 面板不可见 100 ms / 窗口隐藏 200 ms）。
- 卡片：工具摘要（完成 / 进行中 / 失败三种动词 + 对象）、Bash（`$` 命令 + 输出，最高 400 px 内滚动，10 s 后才显示计时）、Edit diff（有 `hunks`
  时显示真实行号，否则照旧）、子智能体、计划 / 待办停靠在输入框上方；审批卡（停靠输入框上方、不占输入框；允许一次 ⌘↵、本会话允许 ⌘⇧↵、拒绝 Esc；
  出现后 400 ms 内回车无效；输入框有草稿时回车只发消息）；提问卡（多题分页、单选 / 多选、"其他"自由输入、跳过）；计划卡（批准并切换模式 / 修改意见）。
- 输入框：一个大圆角框，左 `+`（图片），右发送 / 停止；框下：权限模式选择、上下文环；排队卡片；状态行（阶段文案、思考计时 15/30/45/60 s 分档、
  标签 150 ms 后才切、每个至少停 650 ms）。
- ACP 智能体也用这一套（没有权限模式选择）。
