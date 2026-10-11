# Desktop UI reference review

Reviewed on 2026-10-11. This iteration focuses on the existing React/TypeScript desktop application and preserves its controller identity, pairings, approval gates, and human takeover flow.

## References and evidence

**NetEase UU Remote:** use [the official site](https://uuyc.163.com/), not similarly named domains. Its HTML identifies the canonical NetEase domain, NetEase download/CDN endpoints, and corporate footer. The official [remote assistance guide](https://uuyc.163.com/features/support/) documents a short sequence: open assistance, share the device ID and verification code, then connect. It also distinguishes frequent trusted-device use from temporary assistance and exposes recent connection history. These are useful product-flow references; no UU desktop implementation source was obtained or reviewed, and advertised latency, frame rate, or privacy behavior was not independently tested.

The [privacy guide](https://uuyc.163.com/features/privacy/) groups privacy controls under the active connection's control center. The [2026-10-10 terminal update](https://uuyc.163.com/blog/20261010-zdgx.html) distinguishes disconnecting from terminating a terminal session and documents reconnecting to retained work. Both suggest clear state and action placement. They do not establish that ai-remote supports screen blanking, persistent terminals, or equivalent behavior.

**Apple Liquid Glass:** [HIG Materials](https://developer.apple.com/design/human-interface-guidelines/materials) separates floating controls/navigation from the content layer. Apply glass sparingly to functional elements; retain standard materials for content and keep labels legible. [Meet Liquid Glass](https://developer.apple.com/videos/play/wwdc2025/219/) adds explicit guidance against stacking glass surfaces and explains reduced transparency, contrast, and motion adaptations.

Actual implementation was inspected in Apple's [Landmarks sample](https://developer.apple.com/documentation/swiftui/landmarks-building-an-app-with-liquid-glass), downloaded from the [official sample archive](https://docs-assets.developer.apple.com/published/a88428e6793e/LandmarksBuildingAnAppWithLiquidGlass.zip):

- `LandmarksSplitView.swift` uses `NavigationSplitView`, global `.searchable`, and an inspector instead of making the whole content view glass.
- `BadgesView.swift` groups coordinated controls in `GlassEffectContainer`, applies `.glassEffect(.regular, in:)`, and assigns `.glassEffectID` for related transitions.
- `LandmarkDetailView.swift` extends rich content behind the navigation area with `.backgroundExtensionEffect()` and groups toolbar actions by purpose.

These APIs are SwiftUI implementation evidence, not APIs available to this React webview. [AppKit adoption guidance](https://developer.apple.com/videos/play/wwdc2025/310/) and [NSGlassEffectView](https://developer.apple.com/documentation/appkit/nsglasseffectview) provide a future native integration path. A CSS treatment here is inspired by the material and hierarchy; it is not Apple's native optical renderer.

## Existing implementation inspected

- `apps/desktop/package.json`: React 19 + TypeScript + Vite, Tauri 2 backend; existing icon and motion libraries.
- `src/App.tsx`: main navigation, device detail, approvals, activity, pairing, welcome, settings, and an immersive session view.
- `src/ui/Shell.tsx`: fixed 272 px sidebar, grouped device states, connection route facts, compact session rail. Search is currently decorative. Many navigation rows use clickable `div` elements without keyboard activation.
- `src/ui/Session.tsx`: remote video is the main subject; a driver capsule exposes takeover, pause, handback, and resume. Preserve these visible controls and their distinct human/agent states.
- `src/lib/store.ts`: state and subscriptions are shared across views; offline subscriptions retry. Route and permission facts come from host state.
- `src/styles/tokens.css`, `v2.css`: existing dark/light tokens, platform fonts, reduced-motion and increased-contrast support. Their headers identify them as synchronized generated assets; prefer a deliberate override stylesheet unless the original generator is recovered.
- `src-tauri/tauri.macos.conf.json`: minimum macOS 13.0. Keep older platform and non-blur fallbacks usable.

## Priorities for this iteration

| Priority | Change | Implementation boundary / acceptance |
| --- | --- | --- |
| 1 | Create a floating glass navigation layer: inset sidebar, compact toolbar, session driver capsule, restrained rim highlights and shadows. | Keep device details, activity, settings and approval text on quiet readable content surfaces. Do not stack blurred glass panels. Check light/dark and a busy remote screen. |
| 2 | Make computer search work and make navigation keyboard operable. | Filter the existing host collection locally, retain state grouping, show no-results text and a clear action. Use real buttons and labeled search input; verify Tab, Enter and Space. |
| 3 | Simplify connecting and returning to a device. | Give pairing a clear ordered flow and distinct connect/observe actions; use existing trusted pairings and activity rather than introduce a second verification system. Never imply disconnected means unpaired. |
| 4 | Keep session control and safety states unmistakable. | Preserve takeover/pause placement, textual driver state, approval visibility and real route facts. An offline or waiting state must remain distinguishable without color or animation. |
| 5 | Modernize proportions and motion coherently. | Use softer concentric corners, platform type, calmer spacing, concise action labels and short feedback transitions. Avoid decorative continuous motion; preserve remote-frame performance. |
| 6 | Provide accessibility and rendering fallbacks. | Test reduced motion, increased contrast, reduced transparency where supported, no-backdrop-filter, narrow desktop sizes, and keyboard focus. Use opaque surfaces if transparency compromises clarity. |

Screen blanking, remote wake, new codec/frame-rate claims, persistent terminal sessions, and a native macOS 26 renderer require separate backend/platform work and target-device validation. This UI iteration should not present them as implemented features.
