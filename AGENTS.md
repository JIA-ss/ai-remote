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

### 协议变化（`screen/1`，向后兼容）

- 请求新增 `codecs: ["av1","hevc","h264"]`（可选，未知名字忽略）；不带时仍是 H.264，旧观看端不受影响。
- 回复新增 `codec`（起始编码）。帧头 `flags` 第 0 位是关键帧，**第 1~2 位是编码**（0 H.264、1 HEVC、2 AV1）；
  旧观看端永远收不到非 0 的编码（它们不发 `codecs`）。编码可能在关键帧处改变（硬件失败回退软件 H.264）。
- 上限：帧率 240、长边 8192、码率 500 Mbps（旧被控端仍按 60 / 3840 / 50 Mbps 截断）。

### 桌面端（Tauri，Windows + Mac 共用）

- `apps/desktop/src/lib/codec.ts`：启动时用 `VideoDecoder.isConfigSupported` 探测 4K 下能硬解的编码（AV1 > HEVC > H.264），
  会话请求时带上。
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

### 下一步（按收益排序）

**Windows（这台机器能测）**
- [ ] 真实端到端：本机 host + 桌面端连自己，看实测帧率/码率/首帧时间，确认 WGC 在 100 Hz 屏上真能到 100 fps。
- [ ] 测 Tauri Channel 在 WebView2 上的帧传输开销（每帧走一次 `ipc://` fetch）；高帧率下如有瓶颈，换本地 WebSocket
      或 WebView2 SharedBuffer。
- [ ] 端到端延迟指标：主机时间戳 + 时钟偏移回显，在会话界面显示"采集→显示"延迟。
- [ ] 自适应码率：写帧阻塞（背压）时降码率/分辨率，恢复后回升；现在只有固定档位。
- [ ] 调 NVENC 参数：`CODECAPI_AVEncCommonQualityVsSpeed`、VBV（现为 2 帧）、文字清晰度（4:4:4 解码端普遍不支持，暂不做）。
- [ ] 光标改为客户端绘制（现在合成进画面，光标移动就要整帧编码）。

**Mac 被控端（需要在 Mac 上做）** — `crates/ar-host/src/stream/encode/mac.rs`、`capture/mac.rs`
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
# 旧的真实屏幕采集 + 编码（会读当前屏幕，只在内存里）
cargo test -p ar-host --release --lib capture_and_encode_the_screen -- --ignored --nocapture
```
