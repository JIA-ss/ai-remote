# 流 host：被控端协议（设计文档第 1 节）

分支 `ws/host-s1`，起点 `0bc1d81`。对应 `docs/agent-workspace.zh-CN.md` 第 1 节，和设计不一致的地方已改回设计文档（第 1 节里的"实现说明"
和"实测"表）。

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
