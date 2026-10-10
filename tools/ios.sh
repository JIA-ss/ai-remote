#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
root="$PWD"
if ! command -v cargo >/dev/null; then export PATH="$HOME/.cargo/bin:$PATH"; fi
mode="${1:-bindings}"
generated="$root/apps/ios/Generated"
case "$mode" in
  bindings|mac-core)
    cargo build -p ar-mobile --features bindgen
    mkdir -p "$generated"
    target/debug/ar-mobile-bindgen generate --library target/debug/libar_mobile.dylib --language swift --out-dir "$generated"
    cp "$generated/AIRemoteCoreFFI.modulemap" "$generated/module.modulemap"
    if [[ "$mode" == mac-core ]]; then
      swiftc -parse-as-library -emit-library -emit-module -module-name AIRemoteCore "$generated/AIRemoteCore.swift" \
        -target arm64-apple-macos14.0 -I "$generated" -L "$root/target/debug" -lar_mobile -Xlinker -rpath -Xlinker "$root/target/debug" \
        -o "$generated/libAIRemoteCore.dylib"
      swiftc -parse-as-library -emit-library -emit-module -module-name AIRemotePhone apps/ios/Sources/PhoneModel.swift \
        apps/ios/Sources/PhoneClient.swift apps/ios/Sources/H264Decoder.swift \
        -target arm64-apple-macos14.0 -I "$generated" -L "$generated" -lAIRemoteCore -Xlinker -rpath -Xlinker "$generated" \
        -o "$generated/libAIRemotePhone.dylib"
      swiftc -parse-as-library -emit-library -emit-module -module-name AIRemotePhone apps/ios/Sources/*.swift \
        -target arm64-apple-macos14.0 -I "$generated" -L "$generated" -lAIRemoteCore -Xlinker -rpath -Xlinker "$generated" \
        -o "$generated/libAIRemotePhone.dylib"
    fi
    ;;
  framework)
    if ! xcrun --sdk iphoneos --show-sdk-path >/dev/null 2>&1; then
      echo "Full Xcode and its iOS platform are required. Command Line Tools alone cannot build an iPhone app." >&2
      exit 1
    fi
    "$0" bindings
    rustup target add aarch64-apple-ios aarch64-apple-ios-sim
    for target in aarch64-apple-ios aarch64-apple-ios-sim; do
      cargo build -p ar-mobile --release --target "$target"
    done
    mkdir -p "$generated/Headers"
    cp "$generated/AIRemoteCoreFFI.h" "$generated/Headers/"
    cp "$generated/AIRemoteCoreFFI.modulemap" "$generated/Headers/module.modulemap"
    if [[ -d "$generated/AIRemoteCoreFFI.xcframework" ]]; then
      mv "$generated/AIRemoteCoreFFI.xcframework" "$generated/AIRemoteCoreFFI.previous.$(date +%s).xcframework"
    fi
    xcodebuild -create-xcframework \
      -library target/aarch64-apple-ios/release/libar_mobile.a -headers "$generated/Headers" \
      -library target/aarch64-apple-ios-sim/release/libar_mobile.a -headers "$generated/Headers" \
      -output "$generated/AIRemoteCoreFFI.xcframework"
    ;;
  *) echo "Usage: tools/ios.sh [bindings|mac-core|framework]" >&2; exit 2 ;;
esac
