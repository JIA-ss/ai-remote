#!/usr/bin/env bash
# The iPhone chat model against the desktop's chat-state cases (see tools/PhoneChatModelTests.swift).
set -euo pipefail
cd "$(dirname "$0")/.."
output="target/phone-chat-model"
mkdir -p "$output"
cp tools/PhoneChatModelTests.swift "$output/main.swift"
xcrun swiftc -swift-version 6 -warnings-as-errors \
  apps/ios/Sources/PhoneModel.swift apps/ios/Sources/PhoneChatModel.swift \
  "$output/main.swift" -o "$output/chat-model-tests"
"$output/chat-model-tests"
