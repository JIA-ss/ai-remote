#!/usr/bin/env bash
# Native macOS development and packaging, including the CLI used by the app.
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ "$(uname -s)" != Darwin ]]; then
  echo "This helper needs macOS." >&2
  exit 1
fi
if ! command -v cargo >/dev/null && [[ -x "$HOME/.cargo/bin/cargo" ]]; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi
if ! command -v cargo >/dev/null; then
  echo "Install Rust with rustup (https://rust-lang.org/tools/install), then retry." >&2
  exit 1
fi
xcode-select -p >/dev/null
mode="${1:-dev}"
if [[ $# -gt 0 ]]; then shift; fi
case "$mode" in dev|build) ;; *) echo "Usage: tools/macos.sh [dev|build] [Tauri arguments]" >&2; exit 2 ;; esac
root="$PWD"
# ScreenCaptureKit's maintained bridge and our app both require macOS 13 or newer.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"
target="$(rustc -vV | sed -n 's/^host: //p')"
profile=release
if [[ "$mode" == dev || " $* " == *" --debug "* ]]; then profile=debug; fi
if [[ "$profile" == release ]]; then
  cargo build -p ai-remote --release
else
  cargo build -p ai-remote
fi
mkdir -p apps/desktop/src-tauri/binaries
for binary in ai-remote ai-remote-hostw; do
  cp "target/$profile/$binary" "apps/desktop/src-tauri/binaries/$binary-$target"
done
export AI_REMOTE_CLI="$root/target/$profile/ai-remote"
cd apps/desktop
if [[ ! -d node_modules ]]; then npm ci; fi
if [[ "$mode" == build ]]; then
  exec npm run tauri -- build --config src-tauri/tauri.macos.bundle.conf.json "$@"
fi
exec npm run tauri -- dev "$@"
