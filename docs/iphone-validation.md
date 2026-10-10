# iPhone controller validation

Status on 2026-10-10: the native controller source is implemented. Device and arm64 simulator binaries compile, and simulator UI acceptance passes. No physical
iPhone remote-session acceptance or App Store release is claimed.

| Evidence | Result | Scope |
| :-- | :-- | :-- |
| Rust embedded controller lifecycle | passed | Close/reopen, stable identity, long iOS-style sandbox paths; runs on macOS |
| Rust controller unit tests and strict Clippy | passed | Shared controller and mobile facade |
| Swift/UniFFI core, SwiftUI views | compiled | macOS SDK; does not establish an iOS SDK build |
| Swift/Rust lifecycle races | passed | Three close/start and three background/resume races on native Mac core; stable identity and usable reopened controller |
| Native VideoToolbox H.264 decoder | passed | Replay of a previously captured real Mac frame, 1920×810; rejects a truncated frame |
| Live Swift phone-core ↔ Mac host test | blocked | Pairing/control requests reached the host, but the Mac was locked and its screen stream correctly refused capture |
| iOS device/simulator SDK compile | passed | [Public CI run 38049375262](https://github.com/JIA-ss/ai-remote/actions/runs/38049375262), source a0c8612, Xcode 16.4 / iOS 18.5; arm64 simulator and unsigned arm64 device |
| Simulator UI acceptance | passed | Same run: launch, invalid ticket/rejection and retained text, background/resume, process relaunch; 1 test / 0 failures |
| Physical iPhone test | deferred | Owner will connect the phone later |

Simulator scope includes initial UI and invalid-ticket handling, not a live remote-desktop session.

Repeatable paths are in [the iPhone README](../apps/ios/README.md). Local evidence is saved under
`target/phone-validation/` and kept out of Git because screen content and pairing tickets can be
private. Replay is a decoder test, not fresh remote-screen or phone-install acceptance.

The repository is public by owner authorization. Public CI uses standard macOS runners to build
the iOS targets and run simulator UI acceptance; its actual run result is recorded separately.
Physical-phone acceptance is deferred until the owner connects the phone. Unknown stages remain
unknown in the README and release copy.

The first SDK run failed on private Git dependency access. Publicly buildable snapshots and
iOS-specific netdev/netwatch patches fixed the dependency stage. Generic simulator builds initially
requested x86_64 without a matching Rust slice; this project currently selects arm64 explicitly.
These failures remain visible in Actions history.

Simulator app artifacts are development builds, not a signed device distribution. CI artifacts
expire after one day; local downloaded xcresult evidence is retained under `target/iphone-ci-validation/`.
