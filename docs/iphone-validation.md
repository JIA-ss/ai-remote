# iPhone controller validation

Status on 2026-10-10: source implementation is present on `codex/iphone-controller`. No iPhone
binary, simulator install, device acceptance or App Store release is claimed.

| Evidence | Result | Scope |
| :-- | :-- | :-- |
| Rust embedded controller lifecycle | passed | Close/reopen, stable identity, long iOS-style sandbox paths; runs on macOS |
| Rust controller unit tests and strict Clippy | passed | Shared controller and mobile facade |
| Swift/UniFFI core, SwiftUI views | compiled | macOS SDK; does not establish an iOS SDK build |
| Native VideoToolbox H.264 decoder | passed | Replay of a previously captured real Mac frame, 1920×810; rejects a truncated frame |
| Live Swift phone-core ↔ Mac host test | blocked | Pairing/control requests reached the host, but the Mac was locked and its screen stream correctly refused capture |
| iOS device/simulator SDK compile | in_progress | Full local Xcode unavailable. Public CI now starts; initial runs exposed iOS-only dependency errors, with fixes under validation |
| Physical iPhone test | deferred | Owner will connect the phone later |

Repeatable paths are in [the iPhone README](../apps/ios/README.md). Local evidence is saved under
`target/phone-validation/` and kept out of Git because screen content and pairing tickets can be
private. Replay is a decoder test, not fresh remote-screen or phone-install acceptance.

The repository is public by owner authorization. Public CI uses standard macOS runners to build
the iOS targets and run simulator UI acceptance; its actual run result is recorded separately.
Physical-phone acceptance is deferred until the owner connects the phone. Unknown stages remain
unknown in the README and release copy.
