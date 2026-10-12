# 远控性能与验收范围

本轮把 macOS 视频链路从轮询截图、CPU 色彩转换和 OpenH264 软件编码，改为原生事件采集与经验证的
硬件编码。手动操作仍经过配对权限、接管轮次、撤销与审计。实现不会把请求帧率当成实测结果。

## 当前链路

- macOS13+：ScreenCaptureKit 事件采集、GPU 按需缩放、队列深度3、最新帧槽；IOSurface 保留到
  VideoToolbox 编码，不在正常硬件路径复制整张 BGRA 画面。要求并查询确认硬件加速，失败回退 OpenH264。
- Windows：Graphics Capture 在自建 D3D11 设备上采集，GPU 视频处理器一步完成缩放与 BGRA→NV12，纹理直接交给
  Media Foundation 硬件编码器（NVENC/AMF/Quick Sync）出 H.264/HEVC/AV1，全程不经 CPU；失败依次回退 CPU 采集与
  OpenH264。观看端在请求里列出能硬解的编码，帧头 flags 第 1~2 位标明编码。实测与实现细节见仓库根目录 `AGENTS.md`。
- H.264 AnnexB 与18字节包头保持兼容。编码输出最多等待一帧；传输写入继续响应停止／关键帧控制，
  2秒写入超时终止当前流，重连可选择较低画质。静止画面也能重新发 IDR 恢复。
- 桌面解码队列以两帧为恢复阈值，拥塞时丢弃过期解码链并请求关键帧；迟到回调不能重画旧画面。
  画面结束后明确提示重连，接管期间丢失画面或坐标就撤下人工输入层。
- iPhone默认请求长边1920、30FPS；最多两个等待压缩帧与一个正在解码的帧，丢链合并请求关键帧。
  界面只保留最新画面，旧订阅清理绑定原控制器，避免重开后的订阅ID重用。
- 手动鼠标支持真实 `pointerup` 的零计数事件。人工输入按序执行、最多32个待发操作；相邻文本合并，
  失败清除未发输入并显示错误，不自动重试可能已经部分执行的操作。交还、失焦、取消和几何变化会撤销未发操作。
  Mac 为同一仍有效接管与点击位置保留 Enigo 的原生连续点击状态；真机双击验收仍待解锁桌面。

## 桌面画质档位

| 档位 | 最大长边 | 请求帧率 | 请求码率 |
| --- | ---: | ---: | ---: |
| 均衡，默认 |1920|60 FPS|12 Mbps|
| 流畅 |2560|120 FPS|25 Mbps|
| 清晰 |3840|60 FPS|40 Mbps|
| 极致 |8192（即原生）|240 FPS（即满刷新率）|150 Mbps|
| 低带宽 |1280|30 FPS|3 Mbps|

2026-10-12 前的被控端会把请求截断到 60 FPS、3840 长边、50 Mbps。

这些是请求上限；不放大小于目标的原始画面。1920／3840 长边只有在16:9源画面上分别对应
1920×1080／3840×2160。会话显示实际解码尺寸、每秒解码画面数和接收码率；预览后端明确标注无实时测量。
静止画面的0 FPS表示没有新增画面，不能单凭它判断断线。尚未提供真实点击到画面反馈的延迟指标。

## 本机编码 A/B

2026-10-11，Apple M4、16GiB，release模式。测试从工作区导入同一编码器源码与依赖；每组使用
静态合成纹理、预热后不按真实采集节奏提交60帧，编码计时后再独立解码所有实际输出。测试前后未发现
Rust／Swift／Clang编译进程。**不包含采集、网络传输或终端显示，不能据此宣称远控帧率或延迟。**

| 输出尺寸 | 路径 | 编码p50 | 编码p95 | 成功输出／解码 |
| --- | --- | ---: | ---: | ---: |
|1920×1080|OpenH264 软件|11.670 ms|14.695 ms|59/59|
|1920×1080|VideoToolbox，BGRA拷贝|6.003 ms|6.297 ms|60/60|
|1920×1080|VideoToolbox，原生缓冲|5.617 ms|5.960 ms|60/60|
|3840×2160|OpenH264 软件|48.779 ms|58.301 ms|58/58|
|3840×2160|VideoToolbox，BGRA拷贝|20.660 ms|21.945 ms|60/60|
|3840×2160|VideoToolbox，原生缓冲|19.523 ms|19.730 ms|60/60|

同机4K原生硬件编码的独立处理能力约51帧／秒，本轮不承诺稳定4K60。
OpenH264会跳帧；表内解码数与实际输出一一对应，不将缺少输出的提交算作成功帧。
各路径的实际编码字节数不同，未做同画质对比，不能声称节省带宽。
报告的进程CPU时间不包含系统VideoToolbox服务与GPU功耗，不能当作整机CPU／功耗节省。

尝试低延迟编码 selector 后，实际硬件状态查询在本机失败，因此保留经验证的标准硬件会话。
负面结果保存在 `target/stream-performance/encoder-ab-low-latency-unavailable.json`。

## 来源与复现

实际检查了固定版本 [ScreenCaptureKit11.0 源码与示例](https://docs.rs/crate/screencapturekit/11.0.0/source/)、
[VideoToolbox0.21 编码源码](https://docs.rs/crate/videotoolbox/0.21.0/source/src/compression/mod.rs)、
共享 [apple-cf0.11](https://docs.rs/crate/apple-cf/0.11.0/source/)。这些是 macOS 原生媒体框架的维护绑定，
未引入外部 FFmpeg 服务或运行时下载。CLT 与完整 Xcode 的 Swift运行库路径均在构建中处理。

```sh
MACOSX_DEPLOYMENT_TARGET=13.0 cargo test --workspace --locked
MACOSX_DEPLOYMENT_TARGET=13.0 cargo clippy --workspace --all-targets --locked -- -D warnings
MACOSX_DEPLOYMENT_TARGET=13.0 cargo test -p ar-host --lib hardware_native_annex_b_roundtrip -- --ignored --nocapture
MACOSX_DEPLOYMENT_TARGET=13.0 AI_REMOTE_BENCH_OUTPUT=target/stream-performance/encoder-ab.json \
  cargo test -p ar-host --lib --release synthetic_encoder_ab -- --ignored --nocapture
node --test tools/desktop-state.test.mjs tools/manual-input.test.mjs tools/stream-ui.test.mjs tools/mcp-install.test.mjs
python3 tools/desktop-ui-smoke.py --output target/stream-performance/browser
tools/macos.sh build --debug --bundles app
python3 tools/ui-smoke.py --out target/stream-performance/native-mac
```

原始报告、源码哈希和日志在 `target/stream-performance/`；内容使用合成画面，不采集个人屏幕。
本轮最终工作区220个Rust测试、Clippy、33个前端回归与38项Camofox界面验收通过；Mac开发应用打包成功。
4个Rust显式测试保留为 ignored，另行执行过硬件编码测试；本机不具备PowerShell的1项前端测试跳过。
Swift模块编译、8帧有效GOP、2000帧过载与恢复、回调内关闭、6次核心生命周期竞态均通过。
等待压缩帧峰值2，过载合并为一次关键帧请求；Mac回放确认原生硬件解码。历史1920×810真机帧回放通过，
这些均不等于iPhone实时远控验收。复现原生解码压力测试可用 `tools/PhoneDecoderBackpressureSmoke.swift`，
合成有效帧路径为 `target/stream-performance/phone-fixture.json`，详细环境记录在 `phone-video-context.json`。
本机原生桌面预检仍记录 `the macOS screen is locked`，无法完成真实采集→网络→显示→操作反馈验收。
物理iPhone与Windows硬件远控性能也尚未实测。
