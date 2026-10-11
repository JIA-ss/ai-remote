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

## View and operate the desktop

Use the upper-right fullscreen button to hide navigation and status bars. Fit keeps the complete
desktop inside the phone's notch and corner safe area; **Fill screen** in the control menu enlarges
it to cover the viewport. Pinch to zoom and use two fingers to pan. Local zoom and pan reuse the
decoded image and retain their focus when the phone rotates.

After taking over, one-finger taps and drags operate the desktop. Letterbox taps are ignored, and
adding a second finger cancels pending remote input. The floating toolbar keeps takeover/handback
and emergency pause available. Open **Keyboard** when needed; close the drawer to recover the
viewing area. Scroll and shortcut keys are in **More controls**.

`tools/test-phone-viewport.sh` checks geometry and host-point mapping. Simulator UI fixtures use
the production session view with an explicitly labelled static image; they validate fullscreen,
safe-area fit, zoom and pointer callbacks. Those fixtures exist only in Debug builds. Real text
delivery, handback and physical-phone acceptance are verified separately.

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

## Install on your own iPhone or iPad

The controller requires iOS/iPadOS 17 or later. It is not published on the App Store or TestFlight.
The `iPhone controller` workflow archives a Release `iphoneos` app with signing disabled, verifies its
arm64 Mach-O device platform, and uploads **`iPhone-device-unsigned-ipa`** before simulator UI tests.
Download that artifact from a run for the revision you want to test and unzip it to obtain
`ai-remote-unsigned.ipa` and `package-metadata.json`. Artifacts are retained for seven days.
The actual CI-generated device IPA has passed these packaging checks; physical-phone installation
and remote-control acceptance still require separate verification.

**This IPA is unsigned and must be signed locally for your device before installation.** Downloading, packaging or successfully
compiling it does not prove on-device installation or remote-control acceptance. The simulator app
artifact targets a different platform and cannot be installed on an iPhone, even though both builds
use arm64. The packager rejects simulator/Catalyst images and unsafe bundle links.

Full Xcode is needed for the source-build path above. To install the available device IPA, you can
instead sign it locally with your own Apple ID using AltServer; a full Xcode installation is not
required for that path. Choose a local signing path:

1. **AltServer (CI IPA):** download AltServer from its [official macOS installation guide](https://faq.altstore.io/altstore-classic/how-to-install-altstore-macos),
   copy `AltServer.app` into `/Applications`, and launch it. Connect an unlocked iPhone over USB
   and trust the computer. Hold **Option** while clicking AltServer's menu-bar icon, choose
   **“Sideload .ipa…”**, and follow the local dialog to select `ai-remote-unsigned.ipa`, the phone,
   and your own Apple ID for development signing. Enter credentials and verification codes only
   in the local signing tool. **Installing AltStore first is not required.** AltServer's
   [official release notes](https://faq.altstore.io/release-notes/altserver) document direct IPA
   sideloading and manual reinstallation every seven days for free-account installations.
   For ongoing refresh, optionally install AltStore using the guide above; AltStore supports
   refreshing sideloaded apps. This project's physical-phone installation and control through
   this route remain unverified.
2. **Xcode (source build):** use the source build above, sign in with your Apple Account in Xcode, choose your
   Personal Team or developer team with automatic signing, connect and trust the phone, enable
   Developer Mode, select the phone as the run destination, and run the app. A free Personal Team
   can test on a personal device; its provisioning profiles expire after seven days and require
   rebuilding/reinstalling. See Apple's [device deployment guide](https://developer.apple.com/documentation/xcode/running-your-app-on-simulated-or-physical-devices)
   and [Personal Team limits](https://developer.apple.com/help/account/basics/about-your-developer-account/).
3. **Sideloadly (optional, unverified):** if a full Xcode installation is unavailable, download the tool from the
   [official Sideloadly website](https://sideloadly.io/), connect and trust the iPhone over USB,
   load `ai-remote-unsigned.ipa`, and use its Apple ID signing mode with your own account to
   sign and install locally. Enter credentials and any verification codes only in the local tool.
   Sideloadly documents free/paid account support and seven-day free-account expiry. This route
   still needs physical-phone acceptance; we have not validated this project's installation through
   Sideloadly.

For development-signed apps, follow Apple's [Developer Mode instructions](https://developer.apple.com/documentation/xcode/enabling-developer-mode-on-a-device):
Settings > Privacy & Security > Developer Mode, restart the phone, then confirm enabling it.
The setting may only appear after initiating pairing. Once installed, allow the app's Local Network
prompt, generate a one-time ticket on the host, paste it into the controller, and compare the safety
code on both devices. Then complete the phone acceptance checklist below.

To package a locally compiled **unsigned device** app without signing or installing anything:

```sh
python3 tools/package-ios-ipa.py \
  target/ios-release/ai-remote.xcarchive/Products/Applications/ai-remote.app \
  --output target/ios-install/ai-remote-unsigned.ipa
```

The archive uses `Payload/ai-remote.app`, preserves framework symlinks and executable permissions,
requires no signing identity, and rejects embedded provisioning profiles. Metadata reports
`signing: local-signing-required`; a linker ad-hoc signature is not a development signing identity.
Distribution signing, TestFlight
uploads and production release packaging are separate work.

## Release build inputs

CI uses the ARM64 `macos-15` image and explicitly selects
`/Applications/Xcode_26.2.app/Contents/Developer`. GitHub's
[official runner image inventory](https://github.com/actions/runner-images/blob/main/images/macos/macos-15-arm64-Readme.md)
lists Xcode 26.2, the iOS 26.2 SDK, and an installed iOS 26.2 simulator runtime. The workflow checks
the selected Xcode and device SDK are at least version 26 before building. It retains simulator
Debug build/UI tests on iOS 26.2.

The device artifact is produced by an unsigned **Release archive**, with debug injection disabled.
CI validates the resulting app's device platform, SDK, version and build number, rejects debug and
preview injection dylibs, then applies the same Mach-O/IPA packaging checks. It also preserves
`iPhone-release-unsigned-archive`, containing `ai-remote-unsigned.xcarchive.zip`, for seven days.
This archive is an input for later distribution signing; it is not an App Store Connect export or
a TestFlight build. No Apple account, certificate generation, signing or upload is performed.

XcodeGen's generated Info.plist maps `CFBundleShortVersionString` to `$(MARKETING_VERSION)` and
`CFBundleVersion` to `$(CURRENT_PROJECT_VERSION)`. Local project defaults are version `0.0.1`, build
`1`. CI sets the build to `github.run_number.github.run_attempt` so rerunning a workflow changes the
build number. A manual workflow dispatch can instead specify `build_number`, using one to three
numeric components with a positive first component, for example `12` or `12.2`. Confirm the chosen
number is newer than any build already uploaded before later enabling distribution.

To prepare an unsigned Release archive locally after generating the XCFramework and Xcode project:

```sh
xcodebuild -project apps/ios/AIRemote.xcodeproj -scheme AIRemote \
  -destination 'generic/platform=iOS' -configuration Release \
  -archivePath target/ios-release/ai-remote.xcarchive \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO ENABLE_DEBUG_DYLIB=NO \
  CURRENT_PROJECT_VERSION=12 archive
```

Distribution signing, the app record, account permissions, privacy declarations and encryption
declarations require separate verification before any TestFlight upload.

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

本机核心编译、协议测试、iOS SDK 编译、模拟器与真机验收是不同证据，不能互相代替。真实 CI
生成的设备 IPA 已通过打包校验，仍需用你自己的 Apple ID 在本机签名。源码编译与 Xcode 安装
路径需要完整 Xcode、签名团队和连接的 iPhone；已有 IPA 可以用官方 AltServer 的 Option 菜单
“Sideload .ipa…” 直接签名安装，无须先装 AltStore，也无须为此安装完整 Xcode。免费账号直装
每七天需重新安装；可选择安装 AltStore 续签。Sideloadly 保留为尚未验证的可选路径。手机仍需
完成开发者模式、应用信任及上方真机验收清单；目前不宣称已在真机安装、正式可用或已上架。

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
