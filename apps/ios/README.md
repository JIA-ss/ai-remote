# ai-remote for iPhone

Native SwiftUI controller over the existing Rust controller via UniFFI. It runs the transport in
the app process, with the same encrypted pairing, MCP gate, activity feed and screen/1 protocol;
it does not launch a desktop CLI or open a new public HTTP control port.

Source implementation: device list, pasted one-time pairing tickets and safety-code comparison,
native H.264 decoding through VideoToolbox, touch click/drag, scroll controls, text/keys, take over,
hand back, pause/resume, activity and denying pending approvals. Face ID **Allow** signing is not
implemented; allow at the host or on a Windows Hello controller. The iPhone is a controller, never
an iOS host exposing arbitrary other apps.

Validation is tracked separately: the shared Swift/Rust modules compile on macOS, and the Rust
controller closes/reopens with a stable identity even at iOS-length sandbox paths. The real Mac
host protocol smoke is `tools/phone-core-smoke.py`. This is not a simulator/device test. Native
iOS SDK builds run locally through Xcode and in the [public iPhone CI](https://github.com/JIA-ss/ai-remote/actions/workflows/ios.yml). CI also launches an iPhone simulator and tests initial pairing/error UI; physical phone acceptance remains required.

## Build

Install full Xcode with the iOS platform, Rust via rustup, and XcodeGen (`brew install xcodegen`).
Command Line Tools alone are not an iOS SDK.

```sh
tools/ios.sh framework
cd apps/ios
xcodegen generate
open AIRemote.xcodeproj
```

Select your signing team and connected iPhone in Xcode, enable Developer Mode on the phone, and
run. To compile without signing:

```sh
xcodebuild -project apps/ios/AIRemote.xcodeproj -scheme AIRemote \
  -destination 'generic/platform=iOS Simulator' ARCHS=arm64 CODE_SIGNING_ALLOWED=NO build
```

The build helper generates Swift bindings and an XCFramework for arm64 iPhone and arm64 simulator.
Generated code and compiled frameworks stay out of Git; the pinned UniFFI version regenerates them.
The simulator slice is arm64; use an Apple Silicon Mac to run it. Intel simulator builds are not
provided by this helper.

## Repeat core validation on macOS

```sh
tools/ios.sh mac-core
cargo test -p ar-mobile
python3 tools/phone-core-smoke.py
```

The smoke uses temporary identities and a disposable Mac host. The Mac must be unlocked and its
actual host executable must have Screen Recording permission; a locked screen is a refused test,
not a reason to bypass the host's gate. Its report records encrypted pairing, take over, rejection
of agent control while a person drives, activity, native decoded video, hand back and reopen.

## Phone acceptance checklist

1. Start a real Mac/Windows host and generate `ai-remote pair-code --level control`.
2. Paste the ticket into the phone; compare the same three-word safety code on both devices.
3. Open the computer and require a fresh decoded frame, not a connecting placeholder.
4. Take over; click/type only in a disposable test window. Verify the actual target received it.
5. Check the agent is blocked while you drive, then hand back and verify it resumes.
6. Background/reopen the app: no lingering views, stable identity and retained pairing.
7. Unpair/revoke at the host and prove the phone can no longer operate it.

The app serializes background teardown with reopening, and uses a short iOS background task for
cleanup. Hand-back requests have a three-second Rust timeout because Swift task cancellation does
not cancel UniFFI futures. The app attempts to hand back before leaving a session. A lost connection can prevent that; check
the host's current driver when reconnecting. Backgrounding stops streams and the embedded node;
the app does not claim unattended background video or push notifications.

# iPhone 控制端

这是复用 Rust 核心的原生 SwiftUI 控制端，配对、控制、审计和屏幕流仍经过现有加密协议与闸门。
首次版本通过粘贴一次性票据配对，支持观看、触控点击/拖动、滚动、键盘输入、接管/交还、暂停、
活动记录和拒绝请求。暂未实现 Face ID 的允许签名、二维码扫描和后台推送。

本机核心编译、协议测试、iOS SDK 编译、模拟器与真机验收是不同证据，不能互相代替。真机需要
完整 Xcode、签名团队、连接的 iPhone 和开发者模式；在最终手机验收前不宣称正式可用或已上架。

To reproduce the native Swift/Rust lifecycle races on Mac after `tools/ios.sh mac-core`:

```sh
swiftc -parse-as-library -target arm64-apple-macos14.0 \
  -I apps/ios/Generated -L apps/ios/Generated -lAIRemotePhone -lAIRemoteCore \
  -Xlinker -rpath -Xlinker "$PWD/apps/ios/Generated" \
  tools/PhoneLifecycleSmoke.swift -o target/PhoneLifecycleSmoke
target/PhoneLifecycleSmoke "$PWD/target/phone-lifecycle"
```

This runs three client close/start races and three store background/resume races against the real
embedded core, requiring stable identity and a usable reopened controller. It does not install an
iPhone application.
