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
| iOS device/simulator SDK compile | not_run | Full local Xcode unavailable; CI job did not start |
| Physical iPhone test | deferred | Owner will connect the phone later |

Repeatable paths are in [the iPhone README](../apps/ios/README.md). Local evidence is saved under
`target/phone-validation/` and kept out of Git because screen content and pairing tickets can be
private. Replay is a decoder test, not fresh remote-screen or phone-install acceptance.

Release order: complete iOS SDK compilation, then install/accept on the simulator and the owner's
phone as available, verify the release-facing claims, polish the homepage, and only then publish
the repository and run public GEO checks. Unknown stages must remain unknown in promotional copy.
