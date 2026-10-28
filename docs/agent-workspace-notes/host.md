# 流 host：被控端协议（设计文档第 1 节）

分支 `ws/host-s1`，起点 `0bc1d81`。对应 `docs/agent-workspace.zh-CN.md` 第 1 节，和设计不一致的地方已改回设计文档（第 1 节里的"实现说明"
和"实测"表）。

## 主干整合追记（2026-10-28）

分叉/正文搜索和三类适配器的模型/推理强度协议已进入本轮 main 整合候选，桌面新建页、Claude 与 Codex 会话输入栏都已接入。当前整合验证与未测条件见 AGENTS.md 顶部；下方各次实验的数量和失败继续属于各自源码基准。

## 做了什么

- **会话元数据**（`chat/mod.rs` 的 `Meta`）：`title` / `title_source`、`first_text`、`last_status` / `last_reason`、`pending`、`pinned`、
  `archived`、`permission_mode`。`chat_sessions`（`Chat::describe`）另给 `last_seq`（= 转录 seq，不落盘）和 `branch`。
  旧 `meta.json` 在 `load()` 里由 `migrate()` 补齐（首条消息、上一轮结果从转录读；Claude 会话记 `bypassPermissions`）。
- **分支**：`chat/git.rs`，从文件夹往上找 `.git`（目录，或 worktree / submodule 的 `gitdir:` 文件），读 `HEAD`，不执行 git；
  `.git` 或 `HEAD` 是符号链接时当没有，文件超过 1 KiB 不读，游离 HEAD 不给分支。
- **工具**：`chat_sessions` 加 `archived`（`true` 只列已归档的；本机 `ai-remote chat status` 仍列全部）；新 `chat_update`
  （admin、默认允许、`non_destructive`，审计 `chat_session`/`update`）；`chat_start` 加 `permission_mode`（只对 Claude 记，非法值拒绝）。
  `ar-controller` 的 `CHANNEL_TOOLS` 加 `chat_update`。
- **事件**（`chat/event.rs`）：`title`、`permission`（另带 `call_id`）、`plan_review`、`resolved`、`mode`；`question.questions`；
  `ToolContent::Diff.hunks`（`Hunk`，每块最多 2000 行、每行最多 2000 字节，最多 200 块）。通知码加 `mode_rejected`、`permission_expired`。
- **会话里的"等人回答"**：`Chat.open`。适配器发出 `permission` / `plan_review` / `question` 时先登记再广播；通道收到 `permit` / `answer`
  时用 `Chat::claim` 原子地取走（第一个回答赢，种类不对也取不走），取不到回 `{"t":"error","code":"resolved","id":…}`。
  驱动收到 `Cmd::Permit` / `Cmd::Answer` 后转给适配器并发 `resolved`（带 `by`），审计 `chat_permission`（谁、哪个、结果，不记内容）。
  轮次结束、进程没了、停进程时把剩下的都 `resolved cancelled`。`meta.pending` 跟着最早那个。
- **上行命令**（`chat_channel.rs`）：`permit`、`answer.answers`、`mode`；回答文本截到 8 KiB，多题最多 16 条。只读票据照旧
  在所有命令前回 `readonly`（抽成 `readonly()`）。`mode` 对非 Claude 回通知 `mode_rejected`；没有进程时直接改 meta 并发 `mode`。
- **hello caps**：`runtimes::Caps` 加 `permissions`、`questions_multi`（只 Claude 为 true；Claude 的 `questions` 也改成 true），
  hello 和 `chat_runtimes` 都带。
- **Claude 适配器**（`adapters/claude.rs`）：去掉 `--disallowedTools AskUserQuestion`，加 `--permission-prompt-tool stdio` 和会话的
  `--permission-mode`。`can_use_tool` 三种处理、`set_permission_mode`、`generate_session_title`、`control_cancel_request`、
  `system init/status` 的 `permissionMode` → `mode`、`tool_use_result.structuredPatch` → `tool_update` 里带 hunks 的 diff。
  命令处理抽成纯函数 `command()`，单测不用起进程。
- **Codex**：`numbered_hunks()` 从 `@@ -a,b +c,d @@` 解析，和原来按块拆的 diff 一一配对，只多了 `hunks`。
- **runner 管道**：`permit` / `mode` / `title` 三种命令行、`answer` 带 `answers`、`launch` 带 `permission_mode`。

## 实测

- 真实 Claude Code 2.1.308 直接驱动（`--model haiku`，每项 1 次，脚本在仓库外）：结果写进设计文档第 1.4 节的表。关键两点：
  bypass 下 AskUserQuestion 仍走 `can_use_tool`；"本会话允许"原样回传建议会把规则写进项目 `.claude/settings.local.json`，
  把 `destination` 改成 `session` 后不写文件、同会话第二次同命令不再问（各 1 次）。
- 经被控端会话的真实链路：`AR_REAL_CLAUDE=$(command -v claude) cargo test -p ar-host --lib real_claude -- --ignored --nocapture`，
  1 次通过（约 9.5 s）：首条消息后约 3 s 来 `title`；提问按题回答后模型回 `YOU CHOSE blue`；Bash 本会话允许后输出 200、
  工作目录里没有 `settings.local.json`；切到 plan 后来 `mode plan`。
- `python3.12 tools/chat-smoke.py --quiet`：全过。`--quiet --real claude=$(command -v claude) --model haiku`：全过（含真实 Claude 的
  打断、续上下文、图片、/pong、steer）。注意本机系统 `python3` 是 3.9，脚本用到 3.10 的参数，要用 `~/.local/bin/python3.12`。

## 坑

- 假 Claude 原来把**任何** `control_request` 当打断；首条消息后的标题请求会把 SLOW 轮次打断、或多出一个失败的 result。
  所有替身（`chat/tests.rs`、`runner/tests.rs` 的 sh 和 PowerShell 版、`tools/chat-smoke.py`、`tools/desktop-chat-e2e.py`）都改成只认
  `interrupt`。
- sh 替身里 `echo '…\n…'` 会把 `\n` 变成真换行，JSON 断行；替身里的计划文本不放 `\n`。Rust 原始字符串里有 `"##` 时要用 `r###"`。
- 我们写回给 Claude 的 `control_response` 会被它回显一次（`--replay-user-messages`），只按自己发出的 `request_id` 前缀
  （`ar-mode-` / `ar-title-` / `ar-init`）认回应。
- 标题可能在轮次结束之后才到（替身和真实 Claude 都是），测试别按"标题→turn_end"的顺序等。
- `tool_update` 里带 hunks 的 diff 是**追加**到 tool_call 已有的 diff 后面（前端按 `content` 追加）：Claude 视图读 hunks 那条；旧的
  行视图会看到同一处改动两次（一条没行号、一条有）。前端分支要在旧视图里去重，或只显示带 hunks 的那条。

## 未测 / 没做

- Windows（runner 的 PowerShell 替身只改了打断识别，round-trip 新测试只在 Unix 跑）；服务模式真机没跑。
- 浏览器演示（`lib/demo.ts`）没动：前端事件类型在 `ws/claude` 分支，这里没有 UI 改动，没有截图。
- `tools/fake-codex-app-server.py` 本来就用 difflib 出带 `@@` 的统一 diff，没改；Codex 的 hunks 只有单测覆盖，没经替身端到端看。
- 真实 Claude 下的 `control_cancel_request`、打断时还有未答请求的情形没实测（适配器先 deny 再 interrupt 是按协议推的）。
- 真实 Claude 在 acceptEdits / plan 下各工具是否问人只测了 Edit（default）和 Bash（default）。

## 新增测试与命令

- `cargo test -p ar-host --lib chat`：
  - `chat::git::tests`（3）：子目录找仓库、worktree 的 gitdir、符号链接 / 大文件。
  - `chat::adapters::claude::tests`（新 6）：多题提问与按题回答、计划审批切模式 / 打回、bypass 外的工具问人（一次 / 本会话 / 拒绝 / 过期）、
    打断与轮次结束作废、模式与标题、structuredPatch。
  - `chat::adapters::codex::tests`：`hunk_headers_give_line_numbers_and_left_out_counts_are_one`，原 diff 测试加 hunks 断言。
  - `chat::tests`（新 5 + 1 忽略）：先到先得与本会话允许与审计、多题与计划切模式与 `mode_rejected`、标题与改名 / 置顶 / 归档、
    hunks 与非 Claude 拒绝模式、旧 meta 迁移；`real_claude_asks_titles_and_switches_modes`（`--ignored`，要 `AR_REAL_CLAUDE`）。
  - `chat::runner::tests::restart::permits_answers_and_modes_cross_the_pipe`。
  - `chat_channel::tests`：命令解析、只读拒绝、`resolved` 错误、答案截断。
- 端到端：`python3.12 tools/chat-smoke.py --quiet [--real claude=$(command -v claude) --model haiku]`。
- `tools/desktop-chat-e2e.py` 的 Claude 替身支持 ASK / PLAN / PERMIT / 标题 / 模式 / RICH 的 structuredPatch；这一流没有跑桌面 e2e
  （没有前端改动）。

## 分叉与正文搜索（2026-10-26，分支 `feat/ws-fork-search`，起点 main `ccbcf37`）

对应规格 SB-29（子会话树）、SB-06（曾用名、深入搜索）、SB-37（归档带子会话时的确认）。我们没有"智能体开子会话"，子会话的来源只有**分叉**。
协议写进设计文档第 1 节和 `docs/tools.md` 5.15（中英）。

### 做了什么

- **`chat_start` 的 `fork_of`**（`ChatManager::fork` → 公共的 `create`）：智能体、目录、模型、`hardened`、权限模式都取父会话，
  给了 `runtime` / `cwd` / `model` / `permission_mode` 就拒绝（工具层）。meta 新字段 `parent`（`chat_sessions` 返回）和
  `fork_from`（父会话的 `proto_session`，只在被控端内部用）。没有自己的标题：界面显示「父标题 的分叉」/「Fork of …」。
- **转录**：`copy_transcript` 两遍逐行读父会话 `events.jsonl`：第一遍找最后一个 `turn_end` 的结束位置，第二遍只复制到那里，
  `seq` 从 1 重新编号、去掉 `title` 行；`turns` 记复制到的最后一轮号（因此首条消息后不再要智能体标题）。父会话正在跑的那一轮不复制。
- **第一次启动**：`launch` 在 `proto_session` 为空、`fork_from` 有值时 `resume = fork_from, fork = true`。Claude：`--resume <id>
  --fork-session`（init 报新的 session id，存为自己的 `proto_session`，此后正常 `--resume` 自己）；Codex：`thread/fork`
  （`threadId`、`cwd`、`approvalPolicy: never`、`sandbox: danger-full-access`、`excludeTurns: true`、可选 `model`；字段来自本机
  codex 0.163 的 `app-server generate-json-schema`），失败时和 resume 失败一样发 `context_lost` 并开新线程；ACP 在 `launch` 里拒绝
  （`fork` 时本来也到不了，工具层已拒）。
- **拒绝**：ACP（没有复制对话的办法）、父会话 `turns == 0` 或没有 `proto_session`（没跑过轮次）。未跑过自己轮次的分叉再分叉时用它的
  `fork_from`（同一份对话）。父会话运行中允许。
- **runner 管道**：分叉的对话放在单独的 `fork_from` 字段，不放 `resume`：旧 runner 不认识这个字段时会开新对话，而不是 `--resume`
  父会话、往父会话的对话里续写。
- **`previous_titles`**：`Meta::retitle` 在用户改名（`chat_update`）和智能体标题替换时把旧标题推入，去重、最多 10 个；改回旧名时它不再算曾用名。
- **`chat_search`**（`chat/search.rs`，工具在 `tools/chat.rs`，`spawn_blocking` 里跑）：按词切（非字母数字分隔，CJK 连续字算一个词），
  一段话（一条 `user`，或同一 `id` 的 `message` 各段拼起来，到 `turn_end` 结算）包含全部词才算命中；只解析含 `"t":"user"` /
  `"t":"message"` 的行；每段最多看 32 K 字符；摘录把空白压成一个空格，命中词前留 50 字符，总长 ≤160（含省略号）；会话按
  `last_activity` 倒序读，`limit` 个就停；预算 1.5 s / 256 MiB，读不完 `truncated: true`。进 `CHANNEL_TOOLS`（7 个）。
- **CLI**：`ai-remote chat fork <id> [--prompt]`、`ai-remote chat search "<词>" [--limit]`。
- **桌面端**（`ui/workspace/`、`lib/workspace.ts`）：左栏按 `nestFamilies` 把分叉挂到父会话下（父会话不在同一列表时留在顶层），
  父行标题后展开箭头（`data-family`，旋转 90°，只有箭头点击有 500 ms 防重复，汇总行和 ←/→ 不防），子行 `--ws-depth` 每级缩进 20px，
  `.ws-family::after` 画 L 形树线（1px 竖线、10px 横线、6px 圆角、top 4px、止于最后一行中线）；隐藏时一行汇总隐藏的子会话数 / 失败 /
  未读 / 运行中，当前打开的子会话照样显示；折叠状态存 `ai-remote.ws.families`；「再显示 N 个」按家族的根截断。
  行菜单、会话页菜单的「分叉」（f，⌘⌥O）按 `forkBlock` 禁用并说明（非 Claude/Codex、没回答过、运行中、已归档）；`workspace.fork`
  调 `chat_start {fork_of}`，旧被控端（缺 `runtime` 字段）报"版本太旧"。归档有子会话的会话先问（`FamilyConfirm`）：「仅此一个」把直接
  子会话记进本机 `ai-remote.ws.detached`（撤销时取回），「全部」逐个归档、合并成一条可撤销的提示。⌘K：`formerTitle` 命中的算主结果，
  第二行「曾用名：…」；≥3 字符时防抖 150 ms 后对每台在线电脑并发 `chat_search`，再过 150 ms 还没回来才显示转圈，`contentHits` 合并、
  去掉主结果里已有的会话，作「在内容中」分组带摘录，点开定位到该会话；`truncated` 时提示没读完。演示后端（`lib/demo.ts`）有两个分叉、
  一个曾用名，支持 `fork_of` 与 `chat_search`。

### 实测（本机 macOS，2026-10-26）

- `cargo test -p ar-host --lib chat` 98 过 1 忽略（新增：`a_fork_copies_the_finished_turns_and_its_agent_forks_the_conversation`、
  `earlier_titles_are_kept_for_search`、`search_finds_what_was_said_newest_session_first`、search.rs 5 项、Codex
  `a_fork_asks_for_a_new_thread_with_the_old_ones_turns`、Claude 参数测试加 `--fork-session`）。大文件用例是 10 MiB（2500 行 × 4 KiB
  工具输出）后一条消息：debug 构建下读完、不截断，耗时在 1.5 s 预算内（单次，没记具体毫秒）。
- `~/.local/bin/python3.12 tools/chat-smoke.py --quiet`（替身）：47 项全过（新增 fork / search / 曾用名 13 项，"每轮都审计"从 8 改成 9）。
- 真实 Claude Code 2.1.308（`--model haiku`）：`chat-smoke.py --quiet --real claude=… --model haiku` 60 项全过（1 次）：父会话记住
  KIWI，`chat fork` 后分叉会话 4.2 s 答出 KIWI；再告诉分叉 MANGO，父会话 1.5 s 只列出 KIWI。
- 真实 Codex 0.163.0-alpha.11.2（`--model gpt-5.6-luna`）：同一脚本 60 项全过（1 次）：分叉 3.6 s 答 KIWI，父会话 1.3 s 只答 KIWI。
  上面这些时间是一次的 `chat send --wait` 墙钟时间，不是分叉本身的开销。
- 桌面端到端（debug 包 + 本机隔离被控端，四份 quiet 配置，`E2E_SCRIPT=tools/desktop-workspace-e2e.js`、`view=devices&shotlang=zh&shottheme=dark`）：
  新增 E 段 20 项（行菜单分叉、树与 L 线、复制的轮次、替身说 `(forked from fake-session)`、折叠与记住、汇总行展开、⌘K 内容命中与父会话不命中、
  曾用名、归档确认「仅此一个」与撤销、结束分叉）。修好后连续两轮 181/181 全过；修之前一轮 180/181（汇总行点击落在箭头的 500 ms 防重复里）。
  这台 Mac 的外接屏睡着时应用报告没有屏幕，脚本拒绝跑（离屏位置无法核对）；用 `caffeinate -d` 包住整个运行。截图 e01–e05 看过。
- `tools/workspace.test.mjs` 新增 6 项、`live-workspace.test.mjs` 补桩；`node --test tools/*.test.mjs` 429 项 427 过 2 跳过。
- vite 演示（端口 1532，camofox；注意 1532 的 IPv4 被别的 worktree 的 vite 占着，本次的在 `[::1]:1532`）：深 / 浅、中 / 英共 9 张，
  `/tmp/ws-fork-shots/`（另有原生 e01–e05 的副本），逐张看过。

### 坑

- 本机系统 `python3` 是 3.9：编辑脚本里有很长的含中文的行时，3.9 的分词器报假的 "Non-UTF-8 code"；用 `~/.local/bin/python3.12`。
- chat-smoke 的替身靠 `stream_event` 分段流出文字，命令行打印的是分段，只在最终 assistant 消息里加的后缀看不到，要加在分段里。
- Codex 替身原来所有通知都写死 `threadId`，分叉后线程换了 id，适配器按线程过滤会把通知全丢掉；改为用当前线程。

### 未测 / 没做

- Windows 与服务模式（runner 管道的 `fork_from`）只有代码和单测路径，没真机跑；旧 runner 的降级行为是推导。
- `chat_search` 的预算在真实大机器上的表现没测（只在单测里用 10 MiB 文件和人造预算验证截断）。
- 被控端不支持"仅此一个"归档时把子会话持久地移到顶层：桌面端只在本机记（`ai-remote.ws.detached`），换台控制端仍按 `parent` 嵌套。
