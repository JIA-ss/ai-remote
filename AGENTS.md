# AGENTS.md

给接手开发的人和 coding agent 的工作笔记。用户文档在 `README*.md` 和 `docs/`；这里只记**当前在做什么、为什么这么做、下一步是什么、踩过的坑**。每次推进后更新本文件，并在提交说明里写清思路。

## 通用约定

- 工具链固定 Rust 1.95.0（`rust-toolchain.toml`），`rustfmt.toml` 行宽 120。提交前：
  `cargo fmt --all`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`、
  `node --test tools/desktop-state.test.mjs tools/manual-input.test.mjs tools/stream-ui.test.mjs tools/mcp-install.test.mjs`、
  `cd apps/desktop && npx tsc --noEmit -p .`。
- CI（`.github/workflows/`）在 `macos-15` 和 `windows-2025` 上跑 Rust 测试/Clippy/前端测试；iOS 有单独的 workflow。
  Mac/iOS 代码只能在 Mac 或 CI 上编译，Windows 上改了 `cfg(target_os = "macos")` 的代码要靠 CI 兜底。
- 换行符：`.gitattributes` 是 `* text=auto eol=lf`，提交时统一转 LF。Windows 上用脚本改文件优先二进制读写
  （`open(p, 'rb')` / `open(p, 'wb')`）。检查换行用 `git ls-files --eol <file>`；Git Bash 里 `grep -c` 查 CR 会误报。
- Windows 上的 Bash 工具对很长的 heredoc（含引号的大段代码）解析不稳定，大段内容先写成文件再处理。
- 文档写数字时分清"实测（条件、样本量）/ 推导 / 未测"，不要把请求值写成实测值。

## 命令行远程操作（2026-10-12，分支 `feat/cli-remote`）

目标：人、脚本和 CI 不经 MCP 也能直接操作被控端，形态参照内部测试机工具的命令行：
`ai-remote exec / job / shot / windows / info / doctor --host / ls / stat / mkdir / rm / push / pull`，
默认被控端，统一的退出码和 `--json`。用户文档：`docs/operations*.md` 4.1 节、`docs/tools*.md` 5.13 节。

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
  超到 44 Mbps；LowDelayVBR 为 12~19 / 17~20 Mbps，编码耗时不变。**画质没有做客观对比（PSNR/VMAF），待补。**

### 协议变化（`screen/1`，向后兼容）

- 请求新增 `codecs: ["av1","hevc","h264"]`（可选，未知名字忽略）；不带时仍是 H.264，旧观看端不受影响。
- 回复新增 `codec`（起始编码）。帧头 `flags` 第 0 位是关键帧，**第 1~2 位是编码**（0 H.264、1 HEVC、2 AV1）；
  旧观看端永远收不到非 0 的编码（它们不发 `codecs`）。编码可能在关键帧处改变（硬件失败回退软件 H.264）。
- 上限：帧率 240、长边 8192、码率 500 Mbps（旧被控端仍按 60 / 3840 / 50 Mbps 截断）。

### 桌面端（Tauri，Windows + Mac 共用）

- `apps/desktop/src/lib/codec.ts`：启动时用 `VideoDecoder.isConfigSupported` 探测 4K 下能硬解的编码（HEVC > AV1 > H.264，
  原因见"码率与丢帧"），会话请求时带上。
- `video.ts`：按帧头编码选 codec 字符串；**配置时必须带 codedWidth/codedHeight**——不带的话 Chromium 的 HEVC 路径
  把显示尺寸报成 1280×720（2026-10-12 实测）。编码或尺寸变化时重建解码器。
- 画质档：均衡 1080p60/12 Mbps（默认）、流畅 1440p120/25 Mbps、清晰 4K60/40 Mbps、**极致 原生/240 fps/150 Mbps**、低带宽。

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
- [ ] 画质客观对比：CBR vs LowDelayVBR、三种编码同码率的 PSNR/VMAF（可用 ffmpeg 解 `target/stream-bench/*.bin`）。
- [ ] 端到端延迟指标：主机时间戳 + 时钟偏移回显，在会话界面显示"采集→显示"延迟。
- [ ] 自适应码率：写帧阻塞（背压）时降码率/分辨率，恢复后回升；现在只有固定档位。
- [ ] 调 NVENC 参数：`CODECAPI_AVEncCommonQualityVsSpeed`、VBV（现为 2 帧）、文字清晰度（4:4:4 解码端普遍不支持，暂不做）。
- [ ] 光标改为客户端绘制（现在合成进画面，光标移动就要整帧编码）。

**Mac 被控端（需要在 Mac 上做）** — `crates/ar-host/src/stream/encode/mac.rs`、`capture/mac.rs`
- [ ] `capture::refresh_hz` 在 Mac 上返回 `None`（帧率只受请求限制）；用 CGDisplayMode 的刷新率补上。
- [ ] `AI_REMOTE_TEST_PATTERN` 只在 Windows 实现；Mac 上可在 `capture/mac.rs` 里用 IOSurface 生成同样的滚动图案。
- [ ] VideoToolbox HEVC（Apple Silicon 全系硬件支持）：`Codec::HEVC`，参数集用 HEVC 的 VPS/SPS/PPS 转 Annex B；
      `hardware_available` 里放开 `Codec::Hevc`；`Encoded.codec` 填 Hevc。AV1 苹果没有硬件编码。
- [ ] 帧率上限：`capture/mac.rs` 里 `max_fps.clamp(1, 60)` 改为 240（ProMotion 120 Hz）；`native_size` 的 3840 上限放开。
- [ ] H.264 从 Baseline 改 High（同码率画质更好，所有解码端都支持），去掉 `max_keyframe_interval` 的周期 IDR。
- [ ] 低延迟码控：`kVTVideoEncoderSpecification_EnableLowLatencyRateControl`（之前在 M4 上硬件状态查询失败，需重试）。
- [ ] 在 Mac 上跑 `tools/webcodecs-decode-check.mjs`（参数传 Chrome 路径）验证 WKWebView/Chrome 的 HEVC/AV1 支持。

**iPhone 主控端** — `apps/ios/Sources/H264Decoder.swift`、`crates/ar-mobile`
- [ ] 请求里带 `codecs: ["hevc","h264"]`，解码器按帧头编码位走 HEVC（`CMVideoFormatDescriptionCreateFromHEVCParameterSets`）。

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

### 下一步

- [ ] 本机验证桌面切换后的恢复：锁屏→解锁、改分辨率、独占全屏游戏进出，看重建要多久、有没有漏帧或黑帧。
- [ ] 光标合成（或者和"光标改为客户端绘制"一起做）。
- [ ] 输入跟随输入桌面（`tools/input/inject.rs`）、服务化与安装计划（`install/windows.rs`），见 `CLAUDE.md`。
- [ ] 服务化以后，在锁屏和 UAC 场景下让 `capture::start` 自动选 DDA，去掉环境变量开关。

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
- [ ] Windows 桌面控制端防火墙：检查 `ai-remote-desktop.exe` 的阻止规则，设置页一键修复（UAC + `netsh`），安装包钩子加放行。
- [ ] 可选固定 UDP 端口 `[network] port`，给能手动端口转发的用户。
- [ ] 用局域网里的另一台机器做跨机器验证：mDNS（断外网）、强制中继（`unstable-relay-only` 构建）看提示。
