# macOS development and validation

The first macOS pass supports the native controller app and basic host tools: pairing, system
information, window enumeration, screenshots, mouse/keyboard input, clipboard, shell/jobs and live
H.264 capture. The app includes its CLI and background-host helper, so it can start its controller
without a checkout or a CLI on PATH. This is still a pre-alpha development build.

Install Xcode Command Line Tools (`xcode-select --install`), Node/npm and
[Rust via rustup](https://rust-lang.org/tools/install). The repository pins Rust 1.95.0. If Cargo is not on PATH, run `source "$HOME/.cargo/env"` first
(the helper also finds the usual rustup installation automatically). From the repository root:

```sh
tools/macos.sh dev
tools/macos.sh build --debug --bundles app
open target/debug/bundle/macos/ai-remote.app
```

`tools/macos.sh build` builds a native release app and DMG. The helper prepares both sidecars before
Tauri packages them. The separate bundle config keeps ordinary Cargo builds/tests independent of
generated sidecar files. This helper builds for the current Mac's architecture; it does not prepare
universal binaries, signing or notarization.

On a Mac used as a **host**, run `ai-remote doctor`. Enable the executable (or the terminal that
launches it, as macOS identifies it) in System Settings → Privacy & Security → Screen Recording
and Accessibility, then restart the host. Grants for a CLI launched by a terminal do not prove
that another copy, signed app or LaunchAgent has the same permission. Doctor checks the running
host's permissions, console session and screen lock. Remote tool calls do not bring up TCC prompts.
Use `ai-remote host install --dry-run` to inspect the existing LaunchAgent plan before installation.

Current limits:

- Remote **Allow** from a Mac is unavailable until the Touch ID/Secure Enclave signer is implemented.
  Mac controllers can deny, take over and hand back; allow at the host or a Windows Hello controller.
  The app disables unavailable enrollment/allow controls and explains this in English and Chinese.
- `focus_window` still supports Windows only; on a Mac use the mouse tools or a mounted accessibility
  server. Built-in `ui_tree`, `ui_read` and `ui_action` now provide native AX element observations and semantic actions; see [contracts and acceptance](structured-ui.md). File tools remain planned.
- Mac live capture uses ScreenCaptureKit on macOS 13+, at up to the display's refresh rate
  (CGDisplayMode; 240 requested frames/second at most) and GPU-scaled Retina pixels up to 8K. A
  single latest-frame slot retains native IOSurface buffers; VideoToolbox hardware encoding is
  preferred (HEVC first when the viewer decodes it, else H.264 High profile; the low-latency rate
  controller where the machine has one; keyframes only on request; a live bitrate change without a
  restart), with OpenH264 fallback. Input uses the selected monitor's point geometry. A viewer that
  asks for `cursor: client` gets the pointer separately (NSCursor's picture at point size) and
  ScreenCaptureKit leaves it out of the picture. A requested rate is not a performance guarantee;
  see the isolated encoder measurements and live-acceptance limits in
  [stream performance](stream-performance.md). `AI_REMOTE_TEST_PATTERN=1920x1080` streams the same
  synthetic scrolling desktop as on Windows (`tools/stream-e2e.py --pattern`), no screen needed.
- The lock screen is served by the ordinary host (a reachable secure desktop: capture and input work,
  approval prompts are refused); the login window after a reboot or logout needs
  `sudo … host install --service`, which installs a system LaunchAgent for the `LoginWindow` session.
  Both are implemented from the window server's session flags and launchd's documented behaviour,
  *not yet verified on a real locked or logged-out Mac*; see [operations.md](operations.md#macos).
- Intel Macs and older macOS versions need separate runtime acceptance. No release is signed or
  notarized by this development workflow.

To repeat native acceptance on a Mac with both permissions already granted:

```sh
tools/macos.sh build --debug --bundles app
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python3 tools/macos-smoke.py
python3 tools/input-control-smoke.py
```

The smoke script requires `swiftc`, `ffmpeg` and `ffprobe`. It creates temporary host/controller
profiles, exercises eight concurrent controller cold starts, pairs once, checks doctor, captures
the real screen, clicks and types into its own disposable Cocoa window (including Chinese and
Command+A), decodes H.264, and snapshots the packaged app's real WKWebView with a restricted PATH.
It stops its test host/controller and fixture, and saves the report, logs and images under
`target/macos-validation/`. It checks that normal startup shows the window, then requires the
off-screen canvas to have the decoded frame's size;
also inspect `native-session.png` to confirm the live screen. It never approves
a risky request or changes the default profile/launch-at-login setup.

`input-control-smoke.py` separately checks takeover of active and queued input,
queue overflow, real stdio MCP cancellation, and fresh input after resume. It
checks for an unlocked desktop before starting its disposable window, and stores
its report, audit and screenshot under `target/input-control-validation/`.
`control-gate-smoke.py` checks actual MCP/QUIC cancellation and pending-approval
withdrawal with temporary shell markers and can also run while the screen is
locked. Source comparisons and implementation decisions are recorded in
[implementation-review.md](implementation-review.md).

# macOS 开发与验收

首次适配覆盖原生控制端，以及被控端的配对、系统信息、窗口枚举、截图、鼠标键盘、剪贴板、
shell/后台任务和 H.264 实时画面。上面的命令可从仓库根目录直接运行；打包时会携带 CLI 和
后台被控端辅助程序，应用不依赖开发目录或 PATH 上另装的 CLI。

Mac 作为被控端时，需要给实际运行的程序授予“屏幕录制”和“辅助功能”权限，重启后再运行
`ai-remote doctor`。不同启动方式与签名的权限需要分别核验。控制端暂未支持触控 ID 签名，
可以拒绝、接管和交还，允许操作需在被控端或 Windows Hello 设备上确认。窗口聚焦、文件工具、
原生无障碍树、Intel/旧版 macOS 验收和正式签名发布不属于这次已验证的范围。

`python3 tools/macos-smoke.py` 使用独立临时配置和自己的原生测试窗口，结束后清理测试进程；
证据保存在 `target/macos-validation/`。应用截图直接来自 WKWebView，无需为了截图再授予录屏
权限；验收检查正常启动时窗口可见，并检查屏幕外渲染的真实解码画布尺寸。网页 UI 验收
仍需使用浏览器单独进行，不能替代原生应用与视频解码验收。

## macOS 27 compiler-plugin compatibility

Cargo profiles keep build scripts and procedural-macro dylibs unstripped. On this Mac, a stripped
`time_macros`/`zeroize_derive` dylib existed and passed codesign checks, but direct `dlopen` failed
with `mis-aligned LINKEDIT string pool`; rustc reported E0463 instead. The build override preserves
these compile-time libraries without changing the application's runtime permission checks.
The issue is also tracked in [rust-lang/rust#110536](https://github.com/rust-lang/rust/issues/110536).
