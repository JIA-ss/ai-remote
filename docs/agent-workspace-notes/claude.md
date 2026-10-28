# Claude 会话页（流 claude）开发笔记

设计见 `docs/agent-workspace.zh-CN.md` 第 2.3 节；行为规格在仓库外 `~/ar-refs/spec/claude-{TR,TC,CP,VS}.md`（只按其行为和参数实现）。
本流前一段（分支 `ws/claude`）已提交：`lib/chat.ts` 的新事件与 reducer、`lib/markdown.ts` 尾块增量解析、`lib/claude.ts` 布局纯函数
与 `tools/claude-view.test.mjs`。本段（分支 `ws/claude-s1`）在其上做界面。

## 当前集成与验收状态（2026-10-24）

- P1 / P2 已随 PR #45 / #47 合入 main `028ccc9` / `1aefe9f`，PR #49 / #50 随后合入 main `1ddacb1`；P3 的 PR #48 已合入 **main `35f54a3`**。产品实现为 `f688335`，本机 macOS 应用/CLI 指纹前缀为 `4d62b9a6` / `3e35eed8`。Claude 整页接线、ANSI/diff 与提问细节均已完成。
- 新包本机 16 轮、894 条功能断言及 162 张替身安全图全部通过，primary 78、secondary 64、补充 20 图逐张复核。新真实 Codex 桌面另 24/24、50.014 秒及 6 张图通过；CLI 同字节的旧被控端真实 44/44 与替身35×2沿用已有基准，没有额外模型测试，旧 43/1 失败仍保留。
- 最终 Windows `40024928145` 为 12 轮 818/818、142 张新安全裁图全部功能与视觉通过；core `40024928040` / native `40024928047` 两平台四项及该聊天 CI 共五项全绿。Windows Python 23/23 实际执行、无跳过，workflow 记录 1.899s。Mac 原图/裁图可本地复验；Windows 原图未下载，原图 SHA/尺寸仅 CI 记录，本地只复验安全裁图。旧 Windows 失败和未选旧图仍按基准独立保留。
- main push 生成 core `40029749650` / native `40029749530`；本交接稿形成时未取得终态，未纳入合入前五项成功证据，终态另看对应运行。交接文档独立 PR 另等自身四项必需检查。main35 尚待两台开发机分别安装/确认，本会话未安装 P3；PR #50 另会话的 Mac 既有安装历史保留。真实 Claude 未登录，物理输入、IME、读屏器、动画节拍/性能及本机完整 Xcode 仍未测或不具备环境。

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

## 未测 / 没做（按用户收益排序）

ANSI 工具输出与 Markdown 围栏、成功 Bash diff 着色、提问行内 Markdown 和选区保护均已实现并完成 macOS 原生补验；正常扫光与系统/根属性条件下减少动态效果的 CSS 回退也已验收，不等于设置页面入口验证，不再笼统列为未做。这不代表全部 Claude 规格已经完成。

1. 将已合入的 main `35f54a3` 交给 Mac 与 Windows 开发机分别安装/确认，再验证真实 Claude 的审批、提问、计划、标题、插话/停止和断线回放整段。当前未登录，本会话未安装 P3；PR #50 另会话的 Mac 安装历史不覆盖本次 main35。
2. 补物理拖选与系统剪贴板、键盘/IME、触屏和读屏器；原生脚本的 DOM Range、合成点击与页内剪贴板截获不等于这些实测。Windows 专用 Claude 细节脚本仍未执行。
3. 改善长对话阅读与判断：历史向上分页、章节、下一步提示、敏感信息遮蔽、思考块的“复制为引用”和粘顶提问仍未完成；Bash 转后台等能力也未提供。
4. 补可见性分档节流、动画节拍/性能、原生材质、标题栏与不同缩放条件。正常动画的计算样式为 3s/running 不等于墙钟节拍或性能实测；本机只有 CLT、没有完整 Xcode，完整 iOS 验收仍需独立环境。

## 新增 / 改动的测试与命令

- 没有新增单元测试文件（布局逻辑的测试在上一段的 `tools/claude-view.test.mjs`）。`node --test tools/*.test.mjs` 全过。
- 桌面 e2e（选择器改为 Claude 视图）：
  `cargo build -p ai-remote && (cd apps/desktop && npx tauri build --debug --no-bundle)`，然后
  `python3 tools/desktop-chat-e2e.py --out /tmp/ws-claude-e2e`、
  `E2E_SCRIPT=tools/desktop-chat-e2e-render.js python3 tools/desktop-chat-e2e.py`、
  `E2E_SCRIPT=tools/desktop-chat-offline-e2e.js python3 tools/desktop-chat-e2e.py`（`CARGO_TARGET_DIR` 不是默认时加 `--cli/--app`）。

## 第二阶段首批追记：输出与提问细节（P2）

- TC-22/23 的 ANSI、反显、CR / BS 覆写、不完整 ESC / OSC 与成功 Bash diff 已实现；终端和可选 Markdown ANSI 渲染共享安全文本解析，
  不生成 HTML 或可执行链接。围栏默认换行，复制保留原源文本；diff 只在 hunk 的声明行数内着色。
- 提问题干、选项和说明使用非交互行内 Markdown。选区只阻止该选项的 detail=1 点击，detail=0 激活可继续选择，问题 / 答案值不被渲染改写。
- 浏览器隔离实际组件 30 项通过，深浅主题共 12 张图已看过；本批定向 Node 三套合计 86 项通过。这些数字与上方历史批次分别记账。
- `tools/desktop-claude-details-e2e.js` 已走真实调试应用、控制端、QUIC、隔离被控端与替身，修正 WebKit 计算样式 API 的检查后
  28 项连续两轮通过；当时的 12 张新图后续已逐张通过，纳入 P2 整批 110 张图的集合、SHA-256 与尺寸复验。首轮标准 `userSelect` 检查失败保留；诊断确认 WebKit 只支持前缀，编译 CSS 的有效值已是 `text`，
  所以只改测试按引擎支持读取计算样式，没有为迎合断言修改产品。检查仍要求受支持字段为 `text`，并继续验证原始答案回传。
- 原生脚本使用 DOM Range、合成 detail=1 和 `HTMLElement.click()` 的 detail=0 路径；只在本页拦截复制并在 finally 恢复，不写真实系统剪贴板。
  这不等于物理拖选、键盘或读屏器验收；真实 Claude 登录与 Windows 专用细节脚本仍未测。后续扫光修复前 P3 macOS 的 162 张图、修复后158张图及主干整合后的独立图集分别记账，不互相替代或重复累加。

## 阶段追记：Windows 扫光可读性与最终交接（2026-10-24，P3）

- Windows `39992295300` 的 WebView2 六套替身各两轮实际完成，共 752 条功能断言；应用和 harness 退出、隔离、离屏与清理通过，136 张安全裁图齐全。但逐张复核发现 8 张运行标签缺字或异常灰底，该版本视觉未通过，不能因 CI 绿色认定产品可交付。
  原图没有上传，CI 记录的原图哈希不能本地复验；本地核对的是裁图 SHA-256、尺寸和集合。
- Chrome 155 默认偏好的最小夹具未复现；强制系统减少动态效果后独立复现了嵌套标签缺字和纯文字灰底。浅色渐变选择器覆盖减少动态效果回退，而回退重置了裁剪、内部文字又仍透明，是可复现的 CSS 缺陷。
  旧 Windows 运行没有记录系统 media 值，不能据相似画面追认当时偏好，也没有证据把 PrintWindow 判为原因。
- `8486866` 只改 CSS：主题差异放在高光变量中，系统与应用的减少动态效果回退同时恢复父子文字、清除渐变；保留正常扫光功能、代码浅底和文本 DOM。另行对齐正常扫光参数，该对齐不是灰块修复的必要条件；截图准备规则没有改变。
- **已测**：修复后的编译 CSS 在 Chrome 155 下，浅/深主题 × 应用减少动态效果开/关 × 系统默认/强制减少共 8 组通过，夹具图已查看；正常动画的计算样式周期为 3s、状态为 running，不是逐帧或墙钟周期实测，动画节拍与性能仍未测；全量 Node 391 项（390 过、1 平台跳过）、TypeScript 和前端构建通过。夹具与静态门槛不替代真实 Tauri 验收。
- **证据入口修正**：本会话已主动请求取消 `39998869206`，因为 JS 动效偏好只写入未上传日志，类型化证据无法独立核对该条件；不把取消记为产品失败或成功。偏好白名单与缺失即失败断言已随 `ef3fa3a` 补入；运行 `40000053673` 终态及后续脚本见文末，产品 CSS 与本机原生二进制仍为 `8486866`。
- **后续状态**：本节 macOS 回归与图审已完成；后续主干整合包和 `613cedd` 新诊断仍按各自基准记账。当时 Windows 失败、新CI、PR #48 合入与装机仍待后续证据；现已完成最终35验收并合入，安装仍待开发机分别确认，见顶部。

## 扫光修复后的本机原生回归（2026-10-24）

摘要等待修正前首批产品基准 `8486866`，六套替身加 Claude 细节及导航均各连续两轮，共 16 轮、834 条断言通过；该首批 158 张图逐张通过，原图与安全裁图 SHA-256、尺寸和集合复验一致。macOS chat 两轮系统偏好实测为不减少动态效果，正常扫光计算周期 3s、running；测试临时设置根属性的减少动态效果回退与原属性恢复均通过，未把它当作设置页面入口验收。旧真实 Codex 结果另记，不追认为该新二进制的真实智能体验收。Windows 工具基准 `ef3fa3a` 的运行 `40000053673` 已结束：754 条通过、4 条失败，138 张裁图逐张视觉通过；chat 两轮实际 systemReduce=true、appReduce=null。原始失败只涉及 idle 后同步读取摘要，后续图可见完整摘要与失败计数，不能当作永久丢失。新脚本 `5eea269` 等同一回合的真实摘要后保留原断言，产品二进制不变，本机 chat 75×2 和 38 张新图均通过；两轮最初已有摘要、等待值为 0 ms，不声称本机复现旧 Windows 时序。`40005026321` 后因整合主干主动取消。该阶段图集用其它五套100、补充20与最新chat38共158张，替换旧chat38，不累计为196。

## 主干整合后的验证边界（2026-10-24，P3）

`427cfec` 完整 LiveWorkspace 整合包的默认对话77、渲染12、离线7、预览31项各两轮通过；Claude细节28项和导航10项也各两轮通过。六套替身142张图、补充20张及真实Codex6张分别逐张复核。目录弹层已保留不透明背景，运行标签、Markdown/diff、引用、离线回执与安全预览按新图审查；真实桌面舞台不在审阅或公开范围。

Windows `40012531342` 两项失败属于工作区首个真实解码帧断言，其余五套各两轮通过，但不能据此记Windows整批成功；142张安全裁图已逐张视觉通过，原图未下载、原图哈希仅为CI记录。`613cedd` 新增安全诊断和受控真实Codex插话精确统计，不改变产品；新workspace161×2已过、56张安全图已逐张通过，`613cedd`阶段选定的历史本机集合为16轮894条断言/162PNG，不重复计入旧workspace图。真实Claude登录后的完整链路、Windows专用细节脚本、物理拖选/系统剪贴板、IME与读屏器仍未测。历史向上分页、章节等产品待办继续保留；已经实现且实测的ANSI/提问细节不再混入“未做”。

## 最新 Windows 失败追记（2026-10-24，`40018173520`）

`613cedd`触发的合并树`fe3f578`六套各两轮共812项通过、6项失败，均在workspace。第1轮metadata未变（120）与首个解码帧（143）失败；第2轮另有inert守卫（117）、恢复ownership（118），以及metadata（120）/首帧（143）失败。旧`40012531342`的816过/2失败与142张安全裁图视觉通过保留；新142张安全裁图现已独立逐张视觉通过，全部裁图SHA、尺寸与文件名集合末次复验一致；功能812过/6失败保持。原图未下载，原图哈希仅为CI记录。

新两轮canvas均0×0且连接存在，secure/WebCodecs=true、document_hidden=false，无会话错误/stream-ended；解码器configure与OperationError分别502/502、503/503次，caught-up均0，GPU→CPU fallback各2次。后续`f688335`针对硬件偏好解码失败后重复重建配置的路径加入最小回退，当时实际Windows恢复仍待新矩阵（后续最终35两轮已通过）；这不证明ownership失败由解码压力造成，后者独立核对异步状态和浏览器事件边界。未改服务上限或略过业务断言，该阶段PR #48仍阻塞；后续最终35通过并已合入，旧失败不覆盖，P3安装仍待确认。

## 解码回退与 ownership 最终验收（2026-10-24，main `35f54a3` / 产品 `f688335`）

- 首次仍使用 `prefer-hardware`；当前解码器发生 `OperationError` / `NotSupportedError` 后，同编码与尺寸的下个关键帧用 `no-preference`，由浏览器决定硬件或软件路径。失败配置历史仅保留本次推流最近 16 个，普通 reset 不重复硬件偏好；数据错误与旧回调不污染新配置。新增 7 项真实模块行为测试；不能仅凭 `no-preference` 配置或单测写成已实测软件解码或 Windows 已恢复，实际 Windows 结果另见下文。
- ownership 只把 title/pin/archive 作为元数据，动态状态点另记；DOM 与 host 核对分别留证，host 查询无条件执行，二者仍严格合取。四阶段只导出静态 phase 与布尔。旧失败缺少细分证据，不追认由事件流、状态点或自动标题造成，也不归因解码压力。
- 键盘探针前按同一真实 host 快照等待自动标题与列表对齐，未就绪即失败，不跳过断言、不改批量撤销基线。`35f54a3` 将共享就绪预算改为隐藏文档 25 秒、可见文档 10 秒；隐藏列表在上次取数后再等 20 秒，25 秒由已知轮询间隔加工程余量推导，不是标题时延实测或网络保证。就绪即继续，独立解码帧检查仍为原 30 秒。
- 静态实现阶段 `f688335` 的全量 Node 423 项中 422 过、1 平台项跳过，TypeScript 通过；Windows 证据测试该阶段在 Mac 为 23 项中 21 过、2 Win32 项跳过；最终 Windows workflow 实际执行 23/23、无跳过，记录 `Ran 23 tests in 1.899s`、`OK`。当前新包功能选定集合为 16 轮、894 条断言，162 张替身图；最后 `35f54a3` workspace 161×2 替换旧 workspace，primary 78、secondary 64 与补充 20 图均已独立逐张通过，原图/裁图 SHA、尺寸与选定集合末次复验通过，不重复累计被替换的旧图。
- 新真实 Codex 桌面在源码 `35f54a3`、同一新产品包下 24/24、50.014 秒通过，6 张新安全图逐张通过（5 个唯一裁图内容），原图/裁图指纹、尺寸与 DOM 安全框末次复验完成。被控端 CLI 字节未变，沿用替身 35×2 与真实受控 44/44；不重复模型测试，不覆盖旧 43/1 失败或追认其根因。
- 当前 core `40024928040` / native `40024928047` 两平台四项及 Windows 聊天 `40024928145` 共五项全部成功。最终 Windows 合并树 `b3c4464` 已核对等于源码 `35f54a3`，12 轮 818/818 通过；142 张新安全裁图已独立逐张通过，裁图 SHA、尺寸与完整集合末次复验一致。原图下载数为 0，原图 SHA 仅 CI 记录，本地只复验裁图。更早 `f688335` core `40023600939` 被新35取代取消，不是产品失败，native `40023600860` 成功；`40023600928` 功能成功但属于更早回退诊断，未选未审的 142 图不计入最终集合。旧 Windows 812/6 与独立 142 图视觉通过继续保留。
- 最终 Windows 两轮 workspace 均实际解码到 1024×768，未见会话/stream-ended 错误；每轮 OperationError 2 次、默认偏好回退 2 次、caught-up 2 次，配置回退后在该环境恢复。`no-preference` 不等于已实测软件解码；检查点等待 0 ms 不代表流启动时延。四阶段 ownership 与 host 元数据布尔两轮全真，不能据此追认旧失败瞬间的具体条件。
- PR #48 已合 main `35f54a3`；main push 生成 core `40029749650` / native `40029749530`；本交接稿形成时未取得终态，未纳入合入前五项成功证据，终态另看对应运行。文档独立 PR 也须等四项必需检查。main35 尚待 Mac 与 Windows 开发机分别安装/确认，本会话未安装 P3；PR #50 另会话 Mac 既有安装历史不覆盖本批。真实 Claude 未登录、物理键鼠/IME/读屏器、动画节拍/性能与本机完整 Xcode 边界继续保留。

## 模型与推理强度（2026-10-26，分支 `feat/ws-model-effort2`）

用户反馈：模型选择按钮没适配。新建页的模型芯片点开只是一个文本框（另一会话已改成菜单，本分支在其上接数据），会话页输入框下面没有模型 / 推理强度选择。
协议见 `docs/agent-workspace.zh-CN.md` 1.6、`docs/tools.md` 5.15。

- **做了什么**：被控端上报模型列表（Claude `initialize`、Codex `model/list`、ACP 配置项）并在会话中换模型 / 档位（Claude
  `set_model` 原地换、换档位重启续接；Codex 下一轮 `turn/start` 带上；ACP 配置项）；新建页和会话页共用 `ui/workspace/ModelMenu.tsx`
  的菜单；会话页下巴右侧 `ui/claude/ModelPicker.tsx`（`Composer` 新增可选 `trail`，Codex 视图不传、不受影响）；左侧权限模式在下巴里
  改成纯文字（图标、箭头只留在菜单里，跳过权限确认不再用警告色，NS-31）。
- **实测**（本机 macOS，各 1 次，花额度）：
  - 真实 Claude Code 2.1.308（`AR_REAL_CLAUDE=… cargo test -p ar-host --lib chat::tests::real_claude_switches_models -- --ignored`）：
    列表 12 个模型（Haiku 4.5 不分档，Opus 4.6 / Sonnet 4.6 没有 xhigh）；默认问「你是哪个模型」答 "Claude Opus 5.5"，`set_model haiku`
    后同一进程答 "Claude Haiku 5.5"，Claude 自己的会话记录里回答的模型依次是 `claude-opus-5-5` → `claude-haiku-5-5`；再换档位 `low`
    后新进程命令行带 `--model haiku --effort low --resume <同一会话>`。这个用例前两次失败是测试自己的错（两次提问用了同一个消息 id，
    第二条被当作重复丢掉），改后通过。
  - 真实 Codex 0.163（`AR_REAL_CODEX=… AR_REAL_CODEX_MODEL=gpt-5.6-luna … real_codex`）：两轮，Codex 的会话记录里 `turn_context`
    依次是 `(gpt-5.6-luna, xhigh)`、`(gpt-5.6-luna, low)`。第一轮是 xhigh，因为当时第一轮的 `turn/start` 早于 `model/list` 的应答发出，
    用了本机 `config.toml` 的档位；之后改成第一轮等列表应答（单测覆盖，真实 Codex 没有再跑）。
- **门槛**（本机，见提交）：Rust fmt / clippy / 全工作区测试；前端 426 项（424 过、2 平台跳过）；tsc；
  `E2E_SCRIPT=tools/desktop-workspace-e2e.js` 连续两轮 182/182（zh 深色、en 浅色；新增 M1–M4 共 21 项）；`tools/chat-smoke.py --quiet` 35/35。
  截图（vite 演示 + 无头 Chrome，深/浅 × 中/英，新建页菜单、会话页菜单、推流旁窄工作区 ⌘⇧E、352 px 窄栏）16 张逐张看过，不进仓库。
- **没做**：CP-56（缓存仍热时换模型 / 档位的确认框）、fast 模式（⌘⌥F）、按环境标强调色与「改用环境的 X」（我们没有环境的概念，
  会话页不标强调色）；Codex 会话页（`ui/codex/`）的输入框没接，接法：hello 的 `models` / `model` / `effort`、`models` / `model`
  事件（`lib/chat.ts` 的 reducer 已存到 `chat.models` / `chat.model`），发 `modelCommand(model, effort)`，下一轮 `turn/start` 生效；
  可直接复用 `ModelMenu.tsx` 的 `modelMenuLines` / `ModelChipText` / `useModelKeys`。Claude 的列表要等智能体在这台被控端上运行过
  一次才有（之前新建页用别名兜底）；Codex 第一次在某台被控端上开会话时新建页没有它的模型和档位可选。Windows、真实窗口里的手感没测。

