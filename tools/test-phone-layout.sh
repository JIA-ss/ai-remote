#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
output="target/phone-layout-validation"
mkdir -p "$output"
xcrun swiftc -swift-version 6 -warnings-as-errors \
  apps/ios/Sources/PhoneAdaptiveLayout.swift tools/PhoneAdaptiveLayoutSmoke.swift \
  -o "$output/layout-smoke"
"$output/layout-smoke"
