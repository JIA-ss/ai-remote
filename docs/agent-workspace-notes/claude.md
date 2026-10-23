# Claude 会话页（流 claude）开发笔记

设计见 `docs/agent-workspace.zh-CN.md` 第 2.3 节；行为规格在仓库外 `~/ar-refs/spec/claude-{TR,TC,CP,VS}.md`（只按其行为和参数实现）。
本流前一段（分支 `ws/claude`）已提交：`lib/chat.ts` 的新事件与 reducer、`lib/markdown.ts` 尾块增量解析、`lib/claude.ts` 布局纯函数
与 `tools/claude-view.test.mjs`。本段（分支 `ws/claude-s1`）在其上做界面。

## 做了什么

- **入口与分派**：`ui/claude/ClaudeConversation.tsx` 导出 `ClaudeConversation`（props `{host, target, onLeave, variant?: "page"|"rail"}`）。
  它只开一次连接（`useConversation`，从 `ui/AgentChat.tsx` 导出），收到 hello 后若是 Codex（`isCodexRuntime`）就交给原面板
  `ConversationPanel`（Codex 走法不变），否则渲染 Claude 视图。推流右栏的对话入口（`AgentChat`）改为渲染
  `<ClaudeConversation variant="rail">`。`Conversation`（`CodexConversation` 在用）行为不变，只是拆成 hook + `ConversationPanel`。
- **转录**（`ClaudeTranscript.tsx`、`ClaudeTurn.tsx`）：居中单列（page 768 px + 两侧 32 px；rail 用栏宽），跟随到底按
  `lib/claude` 的 `FOLLOW`（到底 2 px、回复开始时离底 ≤8 px 接回、>200 px 出 36 px 圆形“跳到最新”、>8 px 出底部渐隐），
  滚轮上滚 / 上翻键 / 拖滚动条向上解除跟随；只挂最近 30 轮，远处轮次 `content-visibility`；查找（⌘F）、引用选中文字沿用面板。
  行：用户气泡（5 % 墨色、>1200 字折叠到 256 px 加遮罩）、正文（无气泡，流式按每步约 22 字、目标滞后 550 ms 放出，
  尾块增量解析 `StreamingMarkdown`，刚开的代码围栏先不显示）、工具与思考成组（运行中或待审批默认展开，结束折成“动词 + 计数”一句，
  ≥5 个短语时前 3 个 + “另有 N 项”；单文件合并句；单个调用直接成行）、子智能体行（运行中显示最近子工具 · 次数）、问答回执卡、
  改动汇总卡（前 3 个文件 + “还有 N 个”）、回合尾（复制、耗时；停止的最后一轮给“继续”）。
- **卡片**（`ToolBody.tsx`）：Bash（`$` 命令卡 + 输出，输出区 400 px 内滚动、长输出先给最后 40 行、运行 ≥10 s 才显示计时）、
  Read（用 `cat -n` 行号起点）、diff（有 `hunks` 时两列真实行号，hunk 间“N 行未改动”；没有时退回旧的算 diff）、待办、计划（批准后带“计划已批准”）、
  其他工具 `key: value` + 输出。被拒 / 被停的调用不显示原始报错，显示一句状态。
- **停靠区**（`Cards.tsx`、`Dock.tsx`）：任务清单（12 步窗口，前后各一句“还有 N 步 / N 项待办”）；最早一张待回答的卡在前、后面最多叠 2 张幽灵卡。
  审批卡：标题“允许 X 运行 …？”（英文里 Bash 的 description 本身就是动作）、命令块、后台 / 沙箱外附注、原因、编辑的 diff 预览；
  Esc 拒绝、⌘↵ 允许一次（输入框有草稿时让给输入框）、⌘⇧↵ 本会话允许、1–9 按按钮；出现后 400 ms 内键和点击都不算；答完焦点回输入框且
  500 ms 内的回车不发送（`Composer` 新增 `holdEnter`）。计划卡：计划正文在卡内可收起，拒绝 / 修改…（Esc，3 行意见框，Enter 发出）/
  “批准，编辑前仍询问”（⌘⇧↵）/ “批准并允许编辑”（⌘↵，分裂按钮菜单里还有“批准并跳过权限确认”）。提问卡：分页、单选 / 多选、“其他”自由输入、
  跳过（`[No preference]`）、⌘↵ 下一题 / 提交；草稿按 qid 保存在内存。
- **输入框**：沿用 `ui/chat/Composer.tsx`（斜杠 / @ / 图片 / 历史 / 排队卡），加 `variant="claude"` 外观（一个大圆角卡，+ 在框内左下、发送 / 停止在右下）、
  `lead` 槽放权限模式选择（只对 Claude 且 hello `caps.permissions` 时出现；跳过权限确认先弹确认框、回合中不让选）、`onDraft`、`escClaimed`（卡片在前时 Esc 归卡片）。
  上下文环沿用 `ContextRing`。
- **状态行**（转录末尾）：`statusOf` 的阶段文案经 150 ms 去抖 + 650 ms 最短停留（`debounceStep` / `dwellStep`），思考按 15/30/45/60 s 换五档，
  ≥2 s 才显示计时与 token；一个暖色小方块做“工作中”。
- **文案**只加在 `lib/strings/{en,zh}.ts` 的 “claude view (cl_)” 标记块里（228 条，中英各一）。样式全在 `styles/claude.css`，类名 `cl-` 前缀，作用域 `.cl-root`。
- **演示后端**（`lib/demo.ts`，只改脚本化回复）：首轮发标题和模式；日期测试的编辑带两段 hunks（真实行号 1–4、22–28）；在会询问的模式里跑测试前发审批
  （拒绝就不跑）；消息含 PLAN 时切到计划模式、提出计划卡；提问改成两题（单选 + 多选）。演示 Claude runtime 声明 `permissions` / `questions_multi`。
- **预览页**：`apps/desktop/claude-preview.html`（仅 dev server，不进 build）单独挂 Claude 视图：
  `?variant=page|rail&theme=dark|light&shotlang=en|zh[&w=宽][&start=opencode]`（`start` 先开一个新演示会话，OpenCode 不能插话，用来看排队）。

## 实测（2026-10-23，本机 macOS，camofox 打开 vite 演示，端口 1432）

- 截图 15 张在 `/tmp/ws-claude-shots/`，逐张看过：page 深色英文（工具组展开、diff + 审批卡、完成、计划卡、停止后继续）、page 浅色中文
  （工具组展开、提问卡“其他”、审批卡）、page 深色中文（排队）、rail 深色中文（提问卡、审批卡）、rail 浅色英文（计划卡、模式菜单、跳过权限确认框）。
  另有 page 深色英文提问卡一张（修复单选圆点前的旧图，`page-dark-en-question.png`）。
- 截图过程中发现并修掉的问题（都在提交说明里）：全局 `.live`、`.nt-chat .dot` 类串样式；流式正文不前进；卡片停靠后转录不贴底；
  运行中组头误写“思考中”；被拒调用写成“运行了”；两次快速点击多选只记一个；模式提示不消失；rail 标题换行。
- 桌面 e2e（真实 debug 应用 + 隔离被控端 + 替身智能体，窗口在屏幕外，`CARGO_TARGET_DIR=/tmp/wsclaude-s1-target`）：
  主脚本 `desktop-chat-e2e.js` 68 项两次全过（第一次 67 项全过后又把“回合尾显示耗时”改成 ≥1 s 才显示，检查跟着改，重跑全过）；
  `-render.js` 10 项：第一次全过，第二次“长消息折叠”3 项失败——脚本在发送后立刻取最后一个气泡，量到的是正被移除的“发送中”副本（高 0），
  改成等到会话里那条消息出现再量，之后连跑 2 次全过；`offline-e2e.js` 6 项两次全过。应用截图在 `/tmp/ws-claude-e2e*`（看过 01、10 两张）。
  这些 e2e 跑的是 rail 形态、系统语言（本机中文）、深色主题。

## 坑

- `.live`、`.dot`、`.open`？——前两个是全局类（`v2.css` / `native.css`），第一次截图组体就被 `.live` 变成 22 px 高的胶囊。新类名一律 `cl-` 前缀或 `is-` 状态。
- 本机 127.0.0.1:1432 被别的进程占着（一个 Python），vite 只绑在 `[::1]:1432`；camofox 访问要用 `http://[::1]:1432/…`。
- Vite 热更新会让 `ClaudeConversation` 重挂、重连、从被控端日志重放：本机答过的“答案”只在本地（被控端 `resolved` 不带答案），重放后回执只能写“已回答”。
  真被控端也一样：重开会话看不到当时选了什么（协议限制，见“未做”）。
- Rust 侧：`TodoWrite` 被适配器转成 `plan` 事件、不是工具调用，所以转录里没有“更新了任务清单”这一行，回合结束且全部完成后清单也不留（`planShown`）。

## 未测 / 没做

- 见 StructuredOutput 的 not_done；主要是：会话页整页形态只在预览页里看过（工作区路由不归本流）；章节、下一步框、密钥遮蔽、ANSI 着色、
  Bash 转后台、思考块的“复制为引用”、粘顶提问、历史向上分页没做；按面板可见性节流（33 / 100 / 200 ms）沿用了 `useConversation` 的“每帧或 100 ms”合并，没有再分档。

## 新增 / 改动的测试与命令

- 没有新增单元测试文件（布局逻辑的测试在上一段的 `tools/claude-view.test.mjs`）。`node --test tools/*.test.mjs` 全过。
- 桌面 e2e（选择器改为 Claude 视图）：
  `cargo build -p ai-remote && (cd apps/desktop && npx tauri build --debug --no-bundle)`，然后
  `python3 tools/desktop-chat-e2e.py --out /tmp/ws-claude-e2e`、
  `E2E_SCRIPT=tools/desktop-chat-e2e-render.js python3 tools/desktop-chat-e2e.py`、
  `E2E_SCRIPT=tools/desktop-chat-offline-e2e.js python3 tools/desktop-chat-e2e.py`（`CARGO_TARGET_DIR` 不是默认时加 `--cli/--app`）。
