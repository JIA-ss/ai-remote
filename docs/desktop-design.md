# 桌面端视觉设计（2026-10-21 重做）

目标：桌面端（`apps/desktop`）看起来像一流的 macOS 原生工具（Arc / Linear / Raycast / Screens 5 /
Jump Desktop 这一档）：克制、靠留白和字重分层、一个强调色、画面优先；会话页能一键进入沉浸全屏。
本文记录调研结论、设计原则、令牌表和参考，供后续改样式的人（或 agent）对照。实现笔记和没做完的事
在 `AGENTS.md` 的"桌面端视觉重做"一节。

## 1. 调研结论

### 1.1 现有样式层怎么处理

`src/styles/tokens.css`、`base.css`、`v2.css`、`v2-pages.css`、`v2-host.css` 头部都标着"Synced from the
design system… Do not edit here"，生成器（`bin/build_tokens.py` / `tokens.json`）不在仓库里。
`liquid-glass.css` 是上一轮叠在上面的覆盖层：壁纸渐变、玻璃模糊、22 px 圆角的"卡片套卡片"、发光描边，
就是用户看到的"审美停在上个世纪"的那套。

决定：**令牌名保留、取值重定义；覆盖层推倒重来。**

- 语义令牌（`--canvas / --layer / --card / --text-1..4 / --stroke-1,2 / --agent / --you / --r-* / --dur-*`）
  在五个生成文件里被引用了几百处，改名等于重写整套样式；保留名字，在新覆盖层里重新赋值，生成文件一行不改。
- `liquid-glass.css` 删除，换成 `native.css`（加载顺序最后）。它只做三件事：重定义令牌、去掉生成层里
  "发光 / 光晕 / 卡片套卡片"的规则、给外壳和会话页新的布局。所有新 class 以 `nt-`（native）开头，
  和生成层的 class 不混。
- radix-ui、motion、lucide-react 已在依赖里。motion 用 `motion/react` 的 `AnimatePresence` 做工具条进出场；
  radix-ui 本轮没用到（下拉仍是原生 `<select>`，原生外观在 mac 上反而更对）。

### 1.2 Tauri 2.12 的 macOS 窗口能力（查本地 cargo registry 源码 `tauri-utils-2.10.1/src/config.rs`、
`tauri-2.12.2/src/window/mod.rs`）

| 能力 | 怎么用 | 本轮用法 |
|---|---|---|
| 透明标题栏 | `titleBarStyle: "Overlay"` + `hiddenTitle: true` + `trafficLightPosition` | 已有，保留 48 px 拖拽条 |
| 窗口材质（vibrancy） | `windows[].transparent: true` + `effects: { effects: ["sidebar"], state: "followsWindowActiveState" }`；Tauri 自带，不需要 `tauri-plugin-window-effects`。`macOSPrivateApi` 在 2.12.1+ 已是 no-op（透明 API 总是开着） | 侧栏贴边、透出桌面材质，内容区用 CSS 画成不透明 |
| 全屏 | `getCurrentWindow().setFullscreen(bool)` / `isFullscreen()`，权限 `core:window:allow-set-fullscreen` / `allow-is-fullscreen` | 会话页沉浸模式；绿灯按钮进的原生全屏也会被同步识别 |
| 不换 Space 的全屏 | `setSimpleFullscreen`（仅 macOS） | 没用：和原生全屏手势、菜单栏自动隐藏行为不一致 |
| 隐藏红绿灯 | 没有直接 API；原生全屏下系统自己隐藏标题栏和红绿灯 | 沉浸模式依赖原生全屏，窗口态不隐藏 |
| Windows 标题栏 | `tauri-plugin-frame`（Snap Layouts），`--tauri-frame-controls-width` | 不动 |

### 1.3 参考产品的沉浸模式

- **Jump Desktop（Mac）**：全屏后无 chrome，鼠标推到屏幕顶边滑出一条控制面板（退出全屏、输入、显示、关闭）；
  用户反馈 Cmd+H 在独立 Space 的全屏下无效，官方建议"总是用同一个 Space"。
  来源：[Jump Desktop 社区帖 1](https://support.jumpdesktop.com/hc/en-us/community/posts/214914583/comments/229957266)、
  [帖 2](https://support.jumpdesktop.com/hc/en-us/community/posts/360042683951/comments/37741268294669)。
- **Screens 5**：Mac 端 5.0 重做了工具栏（[MacStories 评测](https://www.macstories.net/reviews/screens-5-an-updated-design-improved-user-experience-and-new-business-model/)）；
  iPhone/iPad 是一个可拖动的圆形浮动工具条，操控远端时自动"缩小存在感"
  （[Edovia 帮助](https://help.edovia.com/screens-5/features/ios-toolbar)）。Mac 端全屏下工具条的具体行为没有公开文档，未核实。
- **Parsec**：画面上叠一个小图标，点开看编码 / 解码延迟、网络往返、码率、分辨率、编解码器
  （[Parsec 官方：Stream Overlay Stats](https://support.parsec.app/hc/en-us/articles/32381603663636-Stream-Overlay-Stats-and-Logging)）；
  第三方资料说 Ctrl+Shift+M 显示延迟，未在官方页面核实。
- **macOS 本身**：全屏 Space 里菜单栏自动隐藏，鼠标推到顶边才下来；RDP 类工具的常见抱怨是去抓远端窗口顶边时误触
  菜单栏（[RoyalApps 论坛](https://support.royalapps.com/support/discussions/topics/17000015683)）。所以我们的顶部浮条
  触发区只取 **顶边 6 px 以内且停留**，底部浮条才是主要操作入口。

本轮采用：原生全屏 + 底部浮动工具条（主要操作）+ 顶部浮条（主机名、退出）+ 右上小 HUD（延迟 / fps / 码率 / 线路）；
鼠标靠顶或底边滑出，3 s 无操作隐藏（指针停在浮条上或显示器 / 画质下拉打开时不隐藏）；接管期间底部正中常驻一个小把手，
悬停或点击唤出浮条；⌃⌘F 切换，Esc 在没接管时退出（接管后 Esc 要发给远端）。

## 2. 设计原则（硬约束）

1. **无边框：两层表面色 + 留白分区。** 侧栏 `--canvas`，内容 `--layer`；卡片、分组、按钮都不描边，靠表面色差和留白成形；
   发丝线（`--stroke-1`）只用在侧栏与内容的分界、分组内行与行之间、表头下面。没有"圆角卡片里再套圆角卡片"，
   没有外发光、光晕、噪点、壁纸（用户 2026-10-21 明确：整体要无边框、现代感）。
2. **8 pt 栅格。** 间距只用 4 / 8 / 12 / 16 / 24 / 32 / 48；控件高度 28（侧栏行、小按钮）/ 32（普通）/ 36（主按钮）。
3. **圆角和阴影统一且克制。** 圆角：控件 6、行 6、卡片 10、浮动层 12。阴影只给浮动层（工具条、菜单、对话框）一档。
4. **字体三套。** 显示 / 正文用系统字体（SF Pro，中文回退 PingFang SC），数据用 Geist Mono；数字一律 `tabular-nums`。
   字号阶梯 11 / 12 / 13 / 15 / 20 / 28，正文 13 px（macOS 标准）。
5. **一个强调色，状态色语义化。** 强调色 = 智能体蓝（`--agent`，也是选中、链接、主按钮）。"你"的琥珀色（`--you`）是产品
   的核心语义（谁在操作），保留。状态：在线绿、空闲灰、中继橙、告警红。
6. **画面优先。** 会话页画面边到边、按比例铺满短边（contain，信箱只在一侧）、边缘落在整设备像素上；所有 chrome 都能退场。
7. **动效 150–250 ms，只做进出场和布局过渡**，`prefers-reduced-motion` 下关掉。

## 3. 令牌表（`native.css` 里的取值）

| 令牌 | 深色 | 浅色 | 用途 |
|---|---|---|---|
| `--canvas` | `#1C1C1E`（侧栏材质透明时不画） | `#F2F2F4` | 侧栏、导航栏底 |
| `--layer` | `#232326` | `#FFFFFF` | 内容区 |
| `--card` | `#2A2A2E` | `#F5F5F7` | 分组容器、设备卡（无描边，靠和 `--layer` 的明度差成形） |
| `--raised` | `#323237` | `#FFFFFF` | 菜单、弹出层 |
| `--well` | `#19191B` | `#F5F5F7` | 代码块、输入底 |
| `--stage` | `#000000` | `#000000` | 画面信箱 |
| `--text-1 / 2 / 3 / 4` | `#F2F2F3 / #A6A6AD / #7C7C85 / #56565E` | `#1D1D1F / #5C5C66 / #7A7A84 / #A3A3AB` | 字 |
| `--stroke-1 / 2` | `rgb(255 255 255 / .08 / .14)` | `rgb(0 0 0 / .08 / .14)` | 发丝线 |
| `--fill-1 / 2` | `rgb(255 255 255 / .06 / .10)` | `rgb(0 0 0 / .04 / .07)` | 悬停、选中底 |
| `--agent`（强调） | `#4C8DFF` | `#0A6CF0` | 智能体、选中、主按钮 |
| `--you` | `#F0B33A` | `#B8760A`（文字）/ `#E8A530`（填充） | 你在操作 |
| `--success` / `--idle` / `--relay` / `--danger` | `#34C759 / #8E8E93 / #FF9F0A / #FF5B5B` | `#1E8E3E / #8E8E93 / #C76A00 / #D93025` | 在线 / 空闲 / 中继 / 告警 |
| `--r-control / --r-row / --r-card / --r-float` | 6 / 6 / 10 / 12 px | 同 | 圆角 |
| `--shadow-float` | `0 8px 24px rgb(0 0 0 / .35), 0 0 0 1px rgb(255 255 255 / .08)` | `0 8px 24px rgb(0 0 0 / .14), 0 0 0 1px rgb(0 0 0 / .06)` | 浮动层唯一的阴影 |
| `--dur-small / --dur-medium` | 150 / 220 ms | 同 | 进出场 |

布局常量：侧栏 240 px；标题拖拽条 48 px；会话页右侧对话栏 `clamp(380px, 34vw, 560px)`；浮动工具条高 44 px、距底 16 px。

## 4. 各页怎么落

- **外壳**：侧栏贴左边、整高、不画边框，右侧一条发丝线；内容区铺满，顶部 48 px 是页面标题栏（也是拖拽区）。
  侧栏项 28 px 高、13 px 字、选中用 `--fill-2` 圆角 6，没有左侧竖条。分组标题 11 px、`--text-3`。
- **设备列表**：标题 + 一行摘要（在线 / 待批准 / 线路，纯文字，数字 20 px）；设备卡片是平的：`--card` 底、
  不描边、圆角 10、无阴影；主按钮用强调色。
- **待批准 / 活动 / 设置 / 电脑页**：跟着令牌走；决策卡去掉琥珀光晕，只留左侧 3 px 琥珀色标；分组容器 `.grp` 不描边，行间发丝线。
  设置页的导航和主侧栏同一套。
- **会话页**：顶栏（返回、主机名、事实行；右侧显示器〔被控端有多块屏时才出现〕、画质、键盘提示、全屏）；舞台黑色边到边；底部浮动工具条
  （一个状态圆点 + 短文案 + 普通尺寸的强调色按钮；线路和延迟只在 HUD）；右上 HUD（延迟 / fps / 码率 / 线路）；右侧直接复用
  智能体工作区的 Claude / Codex 会话，不再有步骤、对话、智能体视角三栏和旧时间线。待审批卡在对话上方独立显示，快捷键只在卡内生效；
  接管时的观看开关和交还说明位于舞台底部的可收起弹层。全屏时只剩舞台和两条浮条；审阅待批操作会退出全屏并定位卡片。

## 5. 验收方式

每页用 `tools/macos.sh build --debug --bundles app` 编出 debug 包，用 computer-use 对真实窗口截图
（深色 / 浅色各一张，`?shottheme=` 不适用于真实窗口，改系统外观），截图放 `docs/images/desktop/`。
会话页用已配对的 `win-dev` 看真实画面。功能回归：`node --test tools/*.test.mjs`、`npx tsc --noEmit -p .`、
`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`。

## 6. 实测发现（2026-10-21，macOS 26 / Tauri 2.12.2 / WKWebView）

- **两个带 `backdrop-filter` 的浮层同时做 transform 动画，WebKit 只画一个**：顶部浮条和底部浮条交替可见，HUD 不受影响。
  浮条改为实色 `rgb(28 28 30 / .96)`，不再模糊。
- **极致档（5K @ 75 fps）全屏时 UI 重绘滞后 5~10 s**：画布变成 5120×2160 设备像素后每帧 `drawImage` 压满合成器，
  页面布局 / 浮条要等几秒（或一次输入事件）才画出来；均衡档（1080p）2.5 s 内就稳定。透明 / 不透明窗口一样，`setFocus`
  唤醒没有用。
- 窗口截图：`app_screenshot` 对这个窗口时好时坏，可靠的做法是整屏截图再按窗口位置裁。

## 7. 玻璃层（2026-10-23，取代第 2 节"内容区不透明"那部分）

用户要求所有界面主打毛玻璃。做法：整窗一层原生材质（macOS `sidebar` NSVisualEffectView / Windows 11 Mica），
`html/body/#root/.win` 透明，内容面是半透明着色（不做 CSS 模糊，磨砂来自原生层）；只有工具栏胶囊、菜单、对话框这类浮层用
CSS `backdrop-filter` + 高光描边；会话舞台上不做任何模糊。开关是 `<html data-material="native|preview|none">`，
`none`（减少透明度、增强对比、强制颜色、Windows 10）时回到第 3 节令牌表的不透明外观。样式在 `src/styles/glass/`，
加载在 `native.css` 之后。导航改成常驻外壳（设置、会话也在里面，侧栏展开 240 / 图标栏 72）。
决定、实测和没做的事见 `AGENTS.md`"桌面端毛玻璃重做 · 第一阶段"。

