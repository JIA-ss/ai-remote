#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
output="target/phone-viewport-validation"
mkdir -p "$output"
xcrun swiftc -swift-version 6 -warnings-as-errors \
  apps/ios/Sources/PhoneViewportGeometry.swift apps/ios/Sources/PhoneModel.swift \
  tools/PhoneViewportGeometrySmoke.swift -o "$output/geometry-smoke"
"$output/geometry-smoke"
