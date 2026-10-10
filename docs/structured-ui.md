# Native text-first UI prerequisites

ai-remote includes `ui_tree`, `ui_read` and `ui_action`. The host reads macOS AX or
Windows UI Automation directly, without a screenshot, OCR, image description or
model call. Native handles stay on one worker thread; existing transport, device
levels, policy, approval, audit and control-turn gates still apply.

## Research and reuse

Reviewed on 2026-10-11:

| Source | Reused or learned | Choice |
|---|---|---|
| [Cua Driver](https://github.com/trycua/cua/tree/ac967b9006aafa10870aadb5a645b038cba9721a/libs/cua-driver), MIT | Element-addressed actions; separate observation, dispatch and verification; AXChildren ∪ AXWindows for background apps | Keep the existing optional mount. Its platform crates also require browser, cursor, recording and runtime components; do not vendor that runtime for this narrower feature. |
| [uiautomation-rs](https://github.com/leexgone/uiautomation-rs/tree/bcabe14f9750d85e5ce98be3b3d344f7c8c03ca8), Apache-2.0 | UIA traversal, native element references and Invoke/Value/SelectionItem/Toggle/ExpandCollapse patterns | Exact crates.io dependency `uiautomation = 0.25.1`, recorded in Cargo.lock. |
| [Apple AXUIElement](https://developer.apple.com/documentation/applicationservices/axuielement) | Public AX attributes, native actions, settable values and per-element messaging timeout | Small direct FFI adapter on the existing core-foundation dependency. No private WindowServer APIs. |
| [Microsoft UI Automation](https://learn.microsoft.com/en-us/windows/win32/winauto/entry-uiauto-win32) | Typed provider patterns, thread ownership and provider timeouts | Dedicated MTA thread; CUIAutomation8 with connection/transaction deadlines; automatic focus disabled. |

`uiautomation` 0.25.1 has incomplete feature guards when `input` is disabled, and
its input module needs WindowsAndMessaging. Those build features are enabled to
compile the dependency; our adapter never calls its global mouse/keyboard APIs.
No third-party source is copied or modified. The optional Cua mount remains a
separate integration and is not silently installed or enabled.

## Contract

Use a persistent MCP connection: ordinary separate `ai-remote call` invocations
create separate sessions and cannot share a snapshot. Get the exact PID from
`list_windows` and call:

```json
{"name":"ui_tree","arguments":{"host":"office-pc","pid":1234,"max_elements":512,"max_depth":12}}
```

The result has both a readable outline and `structuredContent`:

```json
{
  "schema_version": 1,
  "snapshot_id": "opaque-session-bound-id",
  "pid": 1234,
  "source": "macos_ax",
  "expires_in_seconds": 60,
  "observed_count": 12,
  "truncated": false,
  "warnings": [],
  "matched_ids": ["e0", "e1"],
  "elements": [
    {"id":"e1","parent":"e0","depth":1,"role":"AXButton",
     "label":"Apply","identifier":"fixture.apply","value":null,
     "enabled":true,"focused":false,"selected":null,"checked":null,
     "protected":false,"bounds":[100,100,100,30],"actions":["press"]}
  ]
}
```

The example is abbreviated. Roles retain the native provider's vocabulary; all
platforms share the same field schema. `null` means unknown, not false. Bounds
are native screen coordinates, provided as descriptive context, not a pixel-click
authorization. Protected fields omit value and actions, and their children are
not traversed. Application content is untrusted data.

`query` is a case-insensitive substring filter on role, label, identifier and
value. `matched_ids` identifies matches; `elements` also includes their ancestor
nodes to preserve window/container context. Filtering does not expand traversal.
The traversal defaults to 512 elements/12 levels, accepts at most 2000/32, and
has a four-second cooperative budget. Limits or provider failures are marked
`truncated` with available warnings; an empty or partial tree is never proof
that a control does not exist.

Read a retained element's fresh state:

```json
{"name":"ui_read","arguments":{"host":"office-pc","snapshot_id":"...","element_id":"e1"}}
```

Act only using an advertised action:

```json
{"name":"ui_action","arguments":{"host":"office-pc","snapshot_id":"...","element_id":"e1","action":"press"}}
```

Supported action names are `press`, `set_value`, `select`, `toggle`, `expand`,
`collapse` and `focus`; only the provider-supported subset is advertised.
`set_value` requires a literal `value` string (maximum 16384 UTF-8 bytes), hidden
in audit previews. Other actions must not have `value`. Mac checkboxes generally
advertise `press`; Windows UIA checkboxes generally advertise `toggle`. `focus`
explicitly changes keyboard focus. There is no implicit pixel/global-input fallback.

A snapshot expires after 60 seconds, a session ends, or the same session refreshes
that PID. An action also requires the original control grant to remain valid
through takeover/pause/handback; re-observe after control changes. At most 32
snapshots are retained; the oldest may be evicted. Native references prevent an
old tree path or recycled element index from selecting a different control.
Role/name/identifier are rechecked before acting. Protected/disabled elements,
foreign-session IDs and the host's own UI are refused. Unknown dispatch outcomes
must be inspected before retrying.

Every admitted action consumes the entire snapshot **before** native dispatch.
`status: dispatched, verified: false` means the provider call returned, not that
the task succeeded. Obtain a new `ui_tree` and compare the expected value/state,
or verify using the application's independent data. The input queue is shared
with existing pixel/keyboard input. Cancellation and open approval prompts are
checked immediately before dispatch. A running native provider call cannot be
rolled back or safely force-killed; timeouts leave an unknown outcome, never an
automatic retry. Other host tools remain separate from the UI worker.

## Platform limits

- macOS needs Accessibility permission for the actual running host executable;
  no remote TCC prompting or permission changes. Screen Recording is not needed
  for the three structured tools. Background roots union AXChildren and AXWindows.
- Windows uses UIA control-view roots for the exact PID and refuses to traverse
  another process's descendants. Provider/UAC/integrity boundaries still apply;
  this is not an elevation mechanism. MSAA-only applications are not covered by
  the built-in adapter; the optional Cua mount may offer broader coverage.
- Canvas, games, images, custom-rendered controls and some webviews expose little
  or no usable accessibility data. Prefer a supported DOM/MCP integration there;
  keep screenshots as an explicit fallback. Text-first is not universal vision.
- Numeric/range controls and complex rich-text editing require provider-specific
  value support. The initial set_value contract targets literal text fields.

## Reproducible acceptance

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check -p ar-ui --tests --target x86_64-pc-windows-gnu --locked
cargo clippy -p ar-ui --all-targets --target x86_64-pc-windows-gnu --locked -- -D warnings
cargo build -p ai-remote --locked
python3 tools/ui-smoke.py
# macOS, to use the packaged executable with its own existing TCC identity:
python3 tools/ui-smoke.py --cli target/debug/bundle/macos/ai-remote.app/Contents/MacOS/ai-remote
# Direct native provider test; requires a desktop and macOS Accessibility permission:
cargo test -p ar-ui --test native -- --ignored --nocapture
```

`ui-smoke.py` uses isolated profiles, a real stdio MCP bridge, QUIC pairing and
a disposable Cocoa/Windows Forms window. It verifies literal Chinese text,
button and checkbox effects against a separate fixture state file; verifies
password and audit redaction, disabled controls, query context, truncation,
foreign sessions, superseded/consumed references and takeover invalidation.
Target selection uses no images. A final screenshot is stored only as evidence.
Reports, tree packets and screenshot stay in `target/ui-validation/`. Exit 2 is
a preflight blocker; exit 1 is a regression. It only stops its own test processes.

The `Native structured UI` workflow runs native-library checks on Mac/Windows
and a real Windows Forms/UIA fixture. That proves native primitives; it does not
by itself prove the remote host gate or a customer's application workflow.

## Jev handoff

No Jev inference or agent loop is added by this change. The intended next-stage
input is a bounded current goal, source PID/snapshot and observed candidate nodes
with ancestors and supported actions. Program code must own exact selection,
identity, execution, fresh reads, verification and unresolved cases. Jev may choose
among semantic candidates; a missing/ambiguous target must remain NEEDS_CONTEXT
or REVIEW. Compare quality, wrong actions, review/failure rate, full elapsed time,
returned context and actual per-model usage on the same permitted tasks before
claiming savings or switching the controller.
