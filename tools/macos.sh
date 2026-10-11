#!/usr/bin/env bash
# Native macOS development and packaging, including the CLI used by the app.
set -euo pipefail
cd "$(dirname "$0")/.."
mode="${1:-dev}"
if [[ $# -gt 0 ]]; then shift; fi
case "$mode" in dev|build) ;; *) echo "Usage: tools/macos.sh [dev|build] [--dry-run] [Tauri arguments]" >&2; exit 2 ;; esac
dry_run=false
tauri_args=()
has_tauri_args=false
for arg in "$@"; do
  if [[ "$arg" == --dry-run ]]; then dry_run=true; else tauri_args+=("$arg"); has_tauri_args=true; fi
done
if $has_tauri_args; then set -- "${tauri_args[@]}"; else set --; fi
root="$PWD"
profile=release
if [[ "$mode" == dev || " $* " == *" --debug "* ]]; then profile=debug; fi
if [[ "$mode" == build && ${APPLE_SIGNING_IDENTITY+x} && -z "${APPLE_SIGNING_IDENTITY}" ]]; then
  echo "APPLE_SIGNING_IDENTITY is empty; unset it for local ad hoc signing, or supply a certificate identity." >&2
  exit 2
fi

# A portable, read-only plan: no compiler, keychain, copy or bundle mutation is run.
if $dry_run; then
  print_command() { printf '%q ' "$@"; printf '\n'; }
  echo "macOS $mode plan ($profile); native target is resolved with rustc -vV when executed."
  if [[ "$profile" == release ]]; then print_command cargo build -p ai-remote --release;
  else print_command cargo build -p ai-remote; fi
  echo "Copy ai-remote and ai-remote-hostw into Tauri's externalBin directory with the native target suffix."
  if [[ "$mode" == build ]]; then
    echo "Tauri signs sidecars and the app inside out before DMG packaging; identity: ${APPLE_SIGNING_IDENTITY:--}"
    print_command npm run tauri -- build --config src-tauri/tauri.macos.bundle.conf.json "$@"
    print_command /usr/bin/codesign --verify --deep --strict "$root/target/$profile/bundle/macos/ai-remote.app"
  else
    print_command npm run tauri -- dev "$@"
  fi
  exit 0
fi
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
# ScreenCaptureKit's maintained bridge and our app both require macOS 13 or newer.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"
target="$(rustc -vV | sed -n 's/^host: //p')"
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
  # signingIdentity '-' makes the maintained bundler seal the complete .app, including
  # Info.plist and resources, before it packages a DMG. APPLE_SIGNING_IDENTITY can select
  # a certificate instead; no custom designated requirement is added here.
  npm run tauri -- build --config src-tauri/tauri.macos.bundle.conf.json "$@"
  /usr/bin/codesign --verify --deep --strict "$root/target/$profile/bundle/macos/ai-remote.app"
  exit 0
fi
exec npm run tauri -- dev "$@"
