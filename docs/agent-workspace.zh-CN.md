# 智能体工作区：设计

目标：桌面端新增「智能体」工作区，交互上和官方 Claude 桌面 app 的代码模式（Code 标签页）一致：左栏按项目列出各台被控端上的会话，
主区是整页对话或新建会话页。Claude（以及 ACP 智能体）会话按 Claude app 的表现做，Codex 会话按 Codex app 的表现做
（Codex 页面由 `feat/codex-chat-view` 提供，入口组件 `CodexConversation`）。推流会话页右侧的对话侧栏继续保留，和工作区共用同一套对话组件。

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

`chat_sessions` 增加可选参数 `archived: bool`（默认只返回未归档的）。

新工具 `chat_update`（admin 级，默认允许，`CHANNEL_TOOLS` 里，不对 MCP 客户端列出）：参数 `session` 加任意几项
`title`（空串表示恢复自动标题）、`pinned`、`archived`。改标题时 `title_source = "user"`。返回更新后的 meta。

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

- `permit` / `answer` 对已经 `resolved` 的 id 回 `{"t":"error","code":"resolved"}`（另一台设备先答了）。
- `mode` 只对 Claude 有效，其他智能体回通知 `mode_rejected`。
- hello 增加 `caps.permissions: true`（支持 permit / mode）、`caps.questions_multi: true`。

### 1.4 Claude 适配器

- 启动参数：去掉 `--disallowedTools AskUserQuestion`；加 `--permission-prompt-tool stdio`；`--permission-mode` 用会话的模式
  （新会话默认 `bypassPermissions`，和现在一致；`chat_start` 新增可选参数 `permission_mode`）。
- `can_use_tool`：
  - `AskUserQuestion` → `question`（带 `questions`），回答后回 `allow`，`updatedInput = {questions, answers}`。
  - `ExitPlanMode` → `plan_review`；批准回 `allow` 并发 `set_permission_mode` 到所选模式；要改回 `deny` 带说明。
  - 其他工具：模式是 `bypassPermissions` 时直接 allow（保留现在把 `run_in_background` 改成 false 的处理）；否则发 `permission`，
    `once` → allow，`session` → allow 并带上请求里的 `permission_suggestions` 作为 `updatedPermissions`，`deny` → deny
    （带 message 时 `interrupt:false`，模型会看到说明；不带时 `interrupt:true`）。
  - 轮次被打断或进程退出时，未答的请求全部 `resolved cancelled`。
- `mode` 命令 → `control_request` `set_permission_mode`。
- 标题：首条消息被接受后发 `control_request` `generate_session_title`（`description` = 首条消息，`persist:false`），
  回应里的 `title` → `title` 事件（`source:"agent"`）；不回或失败就不发（界面用首条消息兜底）。
- diff：读 user 消息顶层的 `tool_use_result.structuredPatch`，作为对应 tool_call 的 `tool_update` 里带 `hunks` 的 diff。

### 1.5 runner（服务模式）

管道协议同步扩展：`LiveCmd::Permit`、`LiveCmd::SetMode`、`Answer` 带 `answers`；新事件经 `Out::Ev` 原样传。

## 2. 桌面端结构

（规格整理完后补全：工作区外壳、左栏、新建会话页、Claude 会话页、输入框、视觉 token。）
