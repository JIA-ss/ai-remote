#!/usr/bin/env python3
"""The desktop app's agent chat panel end to end, on a Mac: an isolated host with stand-in agents, the
debug desktop app on its own profile, and a script (tools/desktop-chat-e2e.js) that drives the real UI in
the app's webview (AI_REMOTE_SMOKE_SCRIPT, debug builds only). The chain is the real one: page → Tauri →
controller agent → QUIC → host → agent process.

"Codex fake" (tools/fake-codex-app-server.py) speaks Codex's app-server JSON-RPC instead, for the Codex
view of the panel (E2E_SCRIPT=tools/desktop-chat-codex-e2e.js): a turn per keyword (EXPLORE, ASK, SLOW,
FAIL, MANY, NOPHASE, COMPACT). With E2E_CODEX=<path to a codex binary> a real Codex is offered too, as
"Codex (path)" (for --real; it uses that Codex's sign-in and quota); E2E_MODEL=<model> starts it with that model.

The Claude stand-ins speak Claude Code's stream-json: "Rich fake" answers a message containing RICH with
reasoning, a plan (TodoWrite), Read, a failing Bash, Grep, an Edit, a Markdown answer and context usage;
SLOW keeps a turn running until the next message (a steer) or Stop; AGENT runs a sub-agent with two steps of its
own; a message with images answers with their types and sizes; it lists /review, /release-notes and the skill
/tidy; FILES writes an HTML page, Markdown and a
PNG into the session's folder and a Markdown file outside it (named in a Write), for the file previews
(tools/desktop-chat-preview-e2e.js, run with E2E_SCRIPT=tools/desktop-chat-preview-e2e.js). "No-steer fake" is the same program
with `steer = false`.

They also speak the control protocol as Claude Code 2.1.308 does (docs/agent-workspace-notes/host.md): the
first message is answered with a title (generate_session_title), set_permission_mode switches the mode (a
`status` line follows), and three keywords ask with can_use_tool: ASK asks two questions (AskUserQuestion,
the second multiple choice), PLAN proposes a plan (ExitPlanMode), PERMIT wants to run a command (Bash, with
a rule to allow it for the session). The turn then ends with what the answer said (a denial with its message).
RICH's Edit result carries a structuredPatch (real line numbers); EDITAT edits with two hunks at lines 22
and 40. FAIL ends the turn with an error; LATER<n> answers by itself n seconds later (8 by default), while
the person looks elsewhere; a first message with DELAYTITLE gets its title 4 s late. The workspace e2e
(tools/desktop-workspace-e2e.js) uses these, with E2E_QUERY=view=devices to start on the computers page.
CODEDETAILS emits successful terminal output and an ANSI Markdown fence; ASKDETAILS asks one formatted
question and echoes its raw answer as JSON (tools/desktop-claude-details-e2e.js).

Every home gets `[network] quiet = true` for the host and controller roles. The default app window
stays off screen; --visible requests a visible window instead. Screenshots land in --out.
Screenshots settle finite entrance/exit animations at their terminal state; infinite live indicators
stay active. This verifies screenshot content, not animation timing or performance.

  cargo build -p ai-remote && (cd apps/desktop && npx tauri build --debug --no-bundle)
  python3 tools/desktop-chat-e2e.py [--out /tmp/desktop-chat-e2e]

macOS (WKWebView snapshot) and Windows (PrintWindow; the webview is told to keep drawing off screen).
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXE = ".exe" if os.name == "nt" else ""
TMP = None if os.name == "nt" else "/tmp"

def outside_displays(geometry: dict) -> bool:
    """A default smoke window must be wholly outside every reported physical display."""
    window, screens = geometry.get("window"), geometry.get("screens")
    if not isinstance(window, dict) or not isinstance(screens, list) or not screens:
        return False
    for rect in [window, *screens]:
        if not isinstance(rect, dict) or any(type(rect.get(k)) is not int for k in ("x", "y", "width", "height")):
            return False
        if rect["width"] <= 0 or rect["height"] <= 0:
            return False
    return all(window["x"] + window["width"] <= screen["x"]
               or screen["x"] + screen["width"] <= window["x"]
               or window["y"] + window["height"] <= screen["y"]
               or screen["y"] + screen["height"] <= window["y"] for screen in screens)


SMOKE_INPUT_ISOLATION = r'''
// Default smoke is driven by injected DOM events. An unrelated native click/key must not drive
// this throwaway window; explicit --visible runs keep real input for the input-method tests.
(() => {
  if (__ALLOW_NATIVE_INPUT__) return;
  const block = (event) => {
    if (!event.isTrusted) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    void window.__TAURI_INTERNALS__.invoke("log_from_page", { level: "smoke",
      message: `ignored external native input: ${event.type}` }).catch(() => {});
  };
  for (const type of ["click", "dblclick", "contextmenu", "pointerdown", "pointerup", "mousedown", "mouseup", "keydown", "keyup", "wheel"])
    document.addEventListener(type, block, { capture: true, passive: false });
})();
'''

SMOKE_TRACE_UI = r'''
(() => {
  const ids = new WeakMap();
  let next = 0, previous = "";
  const id = (el) => { if (!el) return null; if (!ids.has(el)) ids.set(el, ++next); return ids.get(el); };
  const state = () => ({ route: document.querySelector('.win')?.dataset.route,
    rail: id(document.querySelector('.nt-rail')), chat: id(document.querySelector('.nt-chat')),
    tab: document.querySelector('.nt-rail .seg [role=tab].sel')?.textContent.trim(),
    view: document.querySelector('.nt-chat')?.className, picker: !!document.querySelector('.nt-chat-picker') });
  const log = (type, value) => window.__TAURI_INTERNALS__.invoke('log_from_page',
    { level: 'e2e', message: `     ui-trace ${type}: ${JSON.stringify(value)}` }).catch(() => {});
  document.addEventListener('click', (event) => {
    const el = event.target instanceof Element ? event.target.closest('.nt-rail .seg [role=tab], .nt-chat-menu [role=menuitem], .nt-chat-menu-btn, .nt-anchor .b, .ws-menu-item, .ws-modes button, .ws-row-more, .ws-tb button') : null;
    if (el) void log('click', { text: el.textContent.trim(), classes: el.className, trusted: event.isTrusted,
      state: state(), stack: String(new Error().stack).replace(/\n/g, ' | ').slice(0, 900) });
  }, true);
  for (const type of ['keydown', 'mouseup']) document.addEventListener(type, (event) => {
    if (type === 'keydown' && !(event.metaKey || event.ctrlKey || event.altKey || ['Escape','ArrowLeft','ArrowRight','Enter'].includes(event.key))) return;
    if (type === 'mouseup' && event.button < 3) return;
    void log(type, { key: event.key, code: event.code, button: event.button, meta: event.metaKey, ctrl: event.ctrlKey,
      alt: event.altKey, shift: event.shiftKey, trusted: event.isTrusted, state: state(),
      target: event.target instanceof Element ? event.target.outerHTML.slice(0, 300) : null });
  }, true);
  new MutationObserver(() => {
    const current = state(), signature = JSON.stringify(current);
    if (signature !== previous) { previous = signature; void log('state', current); }
  }).observe(document.documentElement, { subtree: true, childList: true, attributes: true, attributeFilter: ['class'] });
})();
'''

SMOKE_PREPARE_SHOT = r'''
// Screenshot-only terminal state for finite entrance/exit animations, not an animation performance test.
window.__smokePrepareShot = async (name) => {
  if (typeof document.getAnimations !== "function") throw new Error("screenshot animation inspection is unavailable");
  const entrances = new Set(["cdx-in", "cdx-out", "cdx-fade", "cdx-grow", "cl-fade", "cl-card-in", "cl-q-in",
    "ws-pop", "ws-reveal", "ws-palette-in", "ws-toast-in"]);
  const value = (v) => Number.isFinite(v) ? v : v == null ? null : String(v);
  const label = (el) => {
    if (!el) return null;
    const classes = typeof el.className === "string" ? el.className : el.className?.baseVal ?? "";
    return `${el.tagName ?? "element"}${el.id ? `#${el.id}` : ""}${classes ? `.${classes.trim().replace(/\s+/g, ".")}` : ""}`;
  };
  const describe = (animation) => {
    const ancestors = [];
    for (let el = animation.effect?.target, depth = 0; el && depth < 8; el = el.parentElement, depth++) {
      const style = getComputedStyle(el);
      if (depth === 0 || style.opacity !== "1" || el.classList?.contains("cdx-reveal")) {
        ancestors.push({ element: label(el), opacity: style.opacity, transform: style.transform });
      }
    }
    return { animation: animation.animationName ?? animation.id ?? "", currentTime: value(animation.currentTime),
      endTime: value(animation.effect?.getComputedTiming().endTime), playState: animation.playState, ancestors };
  };
  const animations = document.getAnimations();
  // A finite streaming sweep is not an entrance/exit animation and must keep its real state too.
  const terminal = animations.filter((animation) => entrances.has(animation.animationName)
    && Number.isFinite(animation.effect?.getComputedTiming().endTime));
  const log = (phase, samples, layout) => window.__TAURI_INTERNALS__.invoke("log_from_page", { level: "e2e",
    message: `     稳定截图的有限动画终态: ${JSON.stringify({ shot: name, phase, samples, layout })}` });
  await log("before", animations.map((animation) => ({ ...describe(animation), finishForScreenshot: terminal.includes(animation) })));
  for (const animation of terminal) animation.finish();
  await Promise.all(terminal.map((animation) => animation.finished));
  // Force style and layout after finishing, before the caller asks the native webview to capture.
  const rect = document.documentElement.getBoundingClientRect();
  const layout = { width: rect.width, height: rect.height, bodyHeight: document.body.offsetHeight };
  await log("after", terminal.map(describe), layout);
};
'''

# Optional CI evidence only. Rectangles come from the mounted page immediately before its named
# screenshot; the normal harness and product do not use this metadata or change their behavior.
SMOKE_CI_CAPTURE_LAYOUT = r'''(() => {
  const prepare = window.__smokePrepareShot;
  const nativeInvoke = window.__TAURI_INTERNALS__.invoke.bind(window.__TAURI_INTERNALS__);
  let checks = 0;
  window.__smokePrepareShot = async (name) => {
    await prepare(name);
    const visible = (el) => el && el.getBoundingClientRect().width > 0 && el.getBoundingClientRect().height > 0
      && getComputedStyle(el).visibility !== "hidden";
    const modal = document.querySelector(".nt-pv-dlg");
    // LiveWorkspace's video SessionPage is a sibling of this complete workspace pane. Never
    // capture their common parent, and do not fall back to an embedded child when the pane hides.
    const liveWorkspace = document.querySelector(".nt-live-workspace > .nt-workspace-pane");
    const rail = document.querySelector(".nt-rail:not(.nt-workspace-pane)");
    const workspace = document.querySelector('.ws:not([data-embedded="true"])');
    const devices = document.querySelector('.win[data-route="devices"]');
    const element = visible(modal) ? modal : visible(liveWorkspace) ? liveWorkspace : visible(rail) ? rail
      : visible(workspace) ? workspace : visible(devices) ? devices : null;
    const kind = !element ? "unavailable" : element === modal ? "preview_dialog" : element === liveWorkspace ? "stream_workspace_ui"
      : element === rail ? "stream_chat_rail" : element === workspace ? "workspace_ui" : "devices_ui";
    const rect = element?.getBoundingClientRect();
    const style = element && getComputedStyle(element);
    await nativeInvoke("log_from_page", { level: "ci-capture-layout", message: JSON.stringify({
      name, kind, viewport: { width: innerWidth, height: innerHeight, dpr: devicePixelRatio },
      rect: rect ? { x: rect.x, y: rect.y, width: rect.width, height: rect.height } : null,
      radius: style ? Math.max(...["borderTopLeftRadius", "borderTopRightRadius", "borderBottomLeftRadius", "borderBottomRightRadius"].map((key) => parseFloat(style[key]) || 0)) : 0
    }) });
  };
  // Tauri's invoke descriptor is read-only. The CI driver calls this separate writable helper,
  // which forwards the real IPC and only delays done for final layout and counted checks.
  window.__smokeCiInvoke = (command, args, ...rest) => {
    if (command === "log_from_page" && args?.level === "e2e" && /^(?:ok  |FAIL\s)/.test(args.message)) checks++;
    if (command === "log_from_page" && args?.level === "done") return (async () => {
      await window.__smokePrepareShot("zz-final");
      await nativeInvoke("log_from_page", { level: "e2e", message: `${checks} checks` });
      return nativeInvoke(command, args, ...rest);
    })();
    return nativeInvoke(command, args, ...rest);
  };
})();'''

def ci_driver_source(source: str) -> str:
    """Connect only the known driver adapter to the CI helper; never mutate Tauri internals."""
    adapter = "const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);"
    if source.count(adapter) != 1:
        raise ValueError("CI driver must expose exactly one supported invoke adapter")
    return source.replace(adapter, "const invoke = (cmd, args) => window.__smokeCiInvoke(cmd, args);", 1)


parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument("--cli", type=Path, default=ROOT / f"target/debug/ai-remote{EXE}")
parser.add_argument("--app", type=Path, default=ROOT / f"target/debug/ai-remote-desktop{EXE}")
parser.add_argument("--out", type=Path, default=Path(tempfile.gettempdir()) / "desktop-chat-e2e")
parser.add_argument("--verbose", action="store_true", help="print everything the app writes to stderr")
parser.add_argument("--visible", action="store_true", help="show the app window and let it take focus (needed by "
                    "scripts that type with real key events, such as tools/desktop-chat-e2e-ime.js)")
parser.add_argument("--secs", type=int, default=300, help="give up after this long")
parser.add_argument("--real", metavar="AGENT", help="drive a real agent installed on this Mac instead (its name as the "
                    "panel lists it, e.g. 'Claude Code'): a small file task, a / command, an image and a steer; uses its sign-in and quota")
options = parser.parse_args()

RICH_AGENT = r'''
import json, sys, threading
lock = threading.Lock()
def out(v):
    # Timers (LATER, DELAYTITLE) write from their own threads: one line at a time.
    with lock:
        sys.stdout.write(json.dumps(v) + "\n"); sys.stdout.flush()
if "--version" in sys.argv:
    print("2.1.0 (Claude Code)"); sys.exit(0)
mode = sys.argv[sys.argv.index("--permission-mode") + 1] if "--permission-mode" in sys.argv else "default"
out({"type": "system", "subtype": "init", "session_id": "fake-session", "skills": ["tidy"], "permissionMode": mode})
OLD = "def quarter_end(year, q):\n    return date(year, q * 3, 31)\n"
ANSWER = """Fixed: the quarter end was always **day 31**.

### What changed
- `quarter_end` takes the month's last day from `calendar.monthrange`.
- Q2 and Q3 export again.

| Quarter | Before | Now |
|:--|:--|:--|
| Q2 | `ValueError` | ok |
| Q3 | `ValueError` | ok |

```sh
python export.py --quarter Q3
```
"""
def stream(mid, kind, text):
    out({"type": "stream_event", "event": {"type": "message_start", "message": {"id": mid}}})
    for i in range(0, len(text), 12):
        d = {"type": "thinking_delta", "thinking": text[i:i+12]} if kind == "thinking" else {"type": "text_delta", "text": text[i:i+12]}
        out({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": d}})
def tool(mid, calls, results):
    out({"type": "assistant", "message": {"id": mid, "model": "claude-fake", "content": calls,
         "usage": {"input_tokens": 1200, "cache_read_input_tokens": 40000, "output_tokens": 600}}})
    out({"type": "user", "message": {"content": results}})
def todo(*states):
    names = ["Reproduce the failure", "Use the month's real last day", "Run the tests"]
    return {"type": "tool_use", "id": "todo" + "".join(s[0] for s in states), "name": "TodoWrite",
            "input": {"todos": [{"content": n, "status": s, "activeForm": n} for n, s in zip(names, states)]}}
def rich():
    stream("r1", "thinking", "The quarter end is built with day 31; June and September have 30 days. Reproduce first.")
    tool("r1", [todo("in_progress", "pending", "pending"),
                {"type": "tool_use", "id": "read1", "name": "Read", "input": {"file_path": "/work/export.py"}},
                {"type": "tool_use", "id": "bash1", "name": "Bash", "input": {"command": "python export.py --quarter Q3"}}],
         [{"type": "tool_result", "tool_use_id": "read1", "content": OLD},
          {"type": "tool_result", "tool_use_id": "bash1", "is_error": True,
           "content": "Traceback (most recent call last):\nValueError: day is out of range for month\n(exit code 1)"}])
    tool("r2", [{"type": "tool_use", "id": "grep1", "name": "Grep", "input": {"pattern": "date\\(year, .*31\\)"}},
                {"type": "tool_use", "id": "edit1", "name": "Edit", "input": {"file_path": "/work/export.py",
                 "old_string": "    return date(year, q * 3, 31)",
                 "new_string": "    last = calendar.monthrange(year, q * 3)[1]\n    return date(year, q * 3, last)"}},
                todo("completed", "completed", "completed")],
         [{"type": "tool_result", "tool_use_id": "grep1", "content": "export.py:2:    return date(year, q * 3, 31)"}])
    # An edit's result comes alone with its patch: the hunk and its real line numbers.
    out({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "edit1", "content": "ok"}]},
         "tool_use_result": {"filePath": "/work/export.py", "oldString": "    return date(year, q * 3, 31)",
                             "newString": "    last = calendar.monthrange(year, q * 3)[1]\n    return date(year, q * 3, last)",
                             "structuredPatch": [{"oldStart": 1, "oldLines": 2, "newStart": 1, "newLines": 3, "lines": [
                                 " def quarter_end(year, q):", "-    return date(year, q * 3, 31)",
                                 "+    last = calendar.monthrange(year, q * 3)[1]", "+    return date(year, q * 3, last)"]}]}})
    stream("r3", "text", ANSWER)
    out({"type": "assistant", "message": {"id": "r3", "model": "claude-fake", "content": [{"type": "text", "text": ANSWER}],
         "usage": {"input_tokens": 1200, "cache_read_input_tokens": 40000, "output_tokens": 600}}})
    out({"type": "result", "subtype": "success", "is_error": False, "result": "Fixed.",
         "usage": {"input_tokens": 1200, "cache_read_input_tokens": 40000, "output_tokens": 600},
         "modelUsage": {"claude-fake": {"contextWindow": 200000}}})
PAGE = """<!doctype html><html><head><title>Preview page</title>
<script>parent.postMessage("script ran", "*"); document.title = "script ran";
// Run with its scripts (its own window), each outcome asks the preview server for a path.
var mark = function (p) { new Image().src = p; };
mark("ran");
var why = function (e) { return encodeURIComponent(String(e && e.message || e).slice(0, 80)); };
try {
  var ipc = window.__TAURI_INTERNALS__;
  if (!ipc) mark("ipc-missing");
  else ipc.invoke("agent_status").then(function () { mark("ipc-allowed"); }, function (e) { mark("ipc-refused?" + why(e)); });
} catch (e) { mark("ipc-threw?" + why(e)); }
try {
  fetch("https://example.com/").then(function () { mark("fetch-ok"); }, function () { mark("fetch-blocked"); });
} catch (e) { mark("fetch-threw?" + why(e)); }
</script></head>
<body style="font:15px system-ui;margin:24px"><h1 id="h">Quarterly report</h1><p>Static text renders.</p>
<img src="https://example.com/beacon.png" alt="remote image"></body></html>"""
NOTES = "# Notes\n\n| Quarter | Pages |\n| --- | ---: |\n| Q1 | 11 |\n\n- **bold** point\n"
def png(w, h):
    import struct, zlib
    rows = b"".join(b"\x00" + b"".join(bytes((x * 255 // w, y * 255 // h, 160)) for x in range(w)) for y in range(h))
    chunk = lambda k, d: struct.pack(">I", len(d)) + k + d + struct.pack(">I", zlib.crc32(k + d) & 0xffffffff)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) +
            chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))
def files():
    import os
    work = os.getcwd()
    out_dir = os.path.join(work, "out")
    outside = os.path.join(os.path.dirname(work), "outside")
    os.makedirs(out_dir, exist_ok=True); os.makedirs(outside, exist_ok=True)
    page, notes, chart = (os.path.join(out_dir, n) for n in ("report.html", "notes.md", "chart.png"))
    named, secret = os.path.join(outside, "named.md"), os.path.join(outside, "secret.md")
    open(page, "w").write(PAGE); open(notes, "w").write(NOTES); open(chart, "wb").write(png(64, 32))
    open(named, "w").write("# Named outside\n"); open(secret, "w").write("# Not named\n")
    tool("f1", [{"type": "tool_use", "id": "w1", "name": "Write", "input": {"file_path": page, "content": PAGE}},
                {"type": "tool_use", "id": "w2", "name": "Write", "input": {"file_path": notes, "content": NOTES}},
                {"type": "tool_use", "id": "w3", "name": "Write", "input": {"file_path": named, "content": "# Named outside\n"}},
                {"type": "tool_use", "id": "b1", "name": "Bash", "input": {"command": "python chart.py > out/chart.png"}}],
         [{"type": "tool_result", "tool_use_id": "w1", "content": "ok"}, {"type": "tool_result", "tool_use_id": "w2", "content": "ok"},
          {"type": "tool_result", "tool_use_id": "w3", "content": "ok"}, {"type": "tool_result", "tool_use_id": "b1", "content": "wrote out/chart.png"}])
    answer = "Done: the [report](out/report.html), notes in `out/notes.md` and a chart `out/chart.png`."
    out({"type": "assistant", "message": {"id": "f2", "content": [{"type": "text", "text": answer}]}})
    out({"type": "result", "subtype": "success", "is_error": False, "result": answer})
def subagent():
    # A sub-agent (the Agent tool) with two steps of its own, then its summary.
    out({"type": "assistant", "message": {"id": "a0", "content": [{"type": "tool_use", "id": "ag1", "name": "Agent",
         "input": {"description": "Find where the quarter end is built", "prompt": "..."}}]}})
    out({"type": "assistant", "parent_tool_use_id": "ag1", "message": {"id": "a1", "content": [
         {"type": "tool_use", "id": "sg1", "name": "Grep", "input": {"pattern": "quarter_end"}},
         {"type": "tool_use", "id": "sr1", "name": "Read", "input": {"file_path": "/work/export.py"}}]}})
    out({"type": "user", "parent_tool_use_id": "ag1", "message": {"content": [
         {"type": "tool_result", "tool_use_id": "sg1", "content": "export.py:1"},
         {"type": "tool_result", "tool_use_id": "sr1", "content": OLD}]}})
    out({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "ag1",
         "content": [{"type": "text", "text": "It is built in `export.py`, line 2."}]}]}})
    out({"type": "assistant", "message": {"id": "a2", "content": [{"type": "text", "text": "The sub-agent found it in export.py."}]}})
    out({"type": "result", "subtype": "success", "is_error": False, "result": "found"})
QUESTIONS = [
    {"question": "Which quarter should the export cover?", "header": "Quarter", "multiSelect": False,
     "options": [{"label": "Q2", "description": "April to June"}, {"label": "Q3", "description": "July to September"}]},
    {"question": "Which formats?", "header": "Formats", "multiSelect": True,
     "options": [{"label": "CSV"}, {"label": "PDF"}, {"label": "XLSX"}]},
]
DETAILS_ANSI = ("\x1b[31mRED16\x1b[0m \x1b[38;5;46mGREEN256\x1b[0m "
                "\x1b[38;2;12;34;56mTRUECOLOR\x1b[0m\n\x1b[31;44;7mINVERSE\x1b[0m\n"
                "old\rnew\nab\bX\nBEFORE_ESCAPE\x1b\nAFTER_ESCAPE\nSAFE_OSC\x1b]0;HIDDEN_OSC\x1b")
DETAILS_DIFF = ("diff --git a/sample.txt b/sample.txt\n--- a/sample.txt\n+++ b/sample.txt\n"
                "@@ -1 +1 @@\n-BEFORE_DIFF\n+AFTER_DIFF\n+OUTSIDE_HUNK\n")
DETAILS_FENCE = "\x1b[31mFENCE_RED\x1b[0m\n" + "WRAPPED_" * 32
DETAILS_QUESTIONS = [{
    "question": "Pick **output** for `sample.txt`", "header": "Output", "multiSelect": False,
    "options": [
        {"label": "**Plain** `sample.txt`", "description": "Keep *raw* text and [the note](https://example.com/notes)."},
        {"label": "**Styled** `sample.txt`", "description": "Keep ~~old~~ colors and **new** details."},
    ],
}]
def code_details():
    tool("details", [
        {"type": "tool_use", "id": "details-ansi", "name": "Bash", "input": {"command": "python ansi_demo.py"}},
        {"type": "tool_use", "id": "details-diff", "name": "Bash", "input": {"command": "git diff -- sample.txt"}},
    ], [
        {"type": "tool_result", "tool_use_id": "details-ansi", "content": DETAILS_ANSI},
        {"type": "tool_result", "tool_use_id": "details-diff", "content": DETAILS_DIFF},
    ])
    done("### Code details\n\n```text\n" + DETAILS_FENCE + "\n```\n\nOutput stays readable.")
dones = iter(range(1, 1 << 30))
def done(text):
    out({"type": "assistant", "message": {"id": "d" + str(next(dones)), "content": [{"type": "text", "text": text}]}})
    out({"type": "result", "subtype": "success", "is_error": False, "result": text})
asks = 0
def ask(kind, tool, inp, **extra):
    # Every request has its own id (a session asks the same thing more than once).
    global asking
    n = str(asks)
    asking = (kind, "cu-" + kind + n)
    out({"type": "control_request", "request_id": "cu-" + kind + n,
         "request": dict({"subtype": "can_use_tool", "tool_name": tool, "input": inp, "tool_use_id": "tu-" + kind + n}, **extra)})
def edit_at():
    # An edit far into the file: two hunks with their real line numbers (22 and 40).
    out({"type": "assistant", "message": {"id": "e1", "content": [{"type": "tool_use", "id": "editat", "name": "Edit", "input": {
         "file_path": "/work/export.py", "old_string": "    return date(year, q * 3, 31)", "new_string": "    return month_end(year, q * 3)"}}]}})
    out({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "editat", "content": "ok"}]},
         "tool_use_result": {"filePath": "/work/export.py", "oldString": "    return date(year, q * 3, 31)",
                             "newString": "    return month_end(year, q * 3)",
                             "structuredPatch": [
                                 {"oldStart": 22, "oldLines": 3, "newStart": 22, "newLines": 3, "lines": [
                                     " def quarter_end(year, q):", "-    return date(year, q * 3, 31)", "+    return month_end(year, q * 3)", " "]},
                                 {"oldStart": 40, "oldLines": 2, "newStart": 40, "newLines": 3, "lines": [
                                     " def month_end(year, month):", "+    import calendar", "     return date(year, month, calendar.monthrange(year, month)[1])"]}]}})
    done("Edited export.py at lines 22 and 40.")
asking = None
slow = None
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("type") == "control_request" and msg["request"].get("subtype") == "initialize":
        out({"type": "control_response", "response": {"subtype": "success", "request_id": msg["request_id"], "response": {"commands": [
            {"name": "review", "description": "Review the changes on this branch", "argumentHint": "<pr>"},
            {"name": "release-notes", "description": "Draft release notes from the commits"},
            {"name": "tidy", "description": "Tidy the code (a skill)"}]}}})
        continue
    if msg.get("type") == "control_request" and msg["request"].get("subtype") == "generate_session_title":
        title = "Fake: " + msg["request"].get("description", "")[:40]
        reply = {"type": "control_response", "response": {"subtype": "success", "request_id": msg["request_id"], "response": {"title": title}}}
        # DELAYTITLE: the title comes 4 s late (the first message stands in for it until then).
        if "DELAYTITLE" in msg["request"].get("description", ""):
            threading.Timer(4.0, out, (reply,)).start()
        else:
            out(reply)
        continue
    if msg.get("type") == "control_request" and msg["request"].get("subtype") == "set_permission_mode":
        mode = msg["request"]["mode"]
        out({"type": "control_response", "response": {"subtype": "success", "request_id": msg["request_id"], "response": {"mode": mode}}})
        out({"type": "system", "subtype": "status", "status": None, "permissionMode": mode})
        continue
    if msg.get("type") == "control_request":
        if slow or asking:
            if slow:
                slow.set()
            slow = asking = None
            out({"type": "result", "subtype": "error_during_execution", "is_error": True})
        continue
    if msg.get("type") == "control_response":
        r = msg["response"]
        if not asking or r.get("request_id") != asking[1]:
            continue
        kind, asking = asking[0], None
        asks += 1
        resp = r.get("response", {})
        if resp.get("behavior") != "allow":
            done("Not done: " + (resp.get("message") or "declined"))
        elif kind == "ask":
            done("You picked: " + "; ".join(q + " -> " + a for q, a in resp["updatedInput"].get("answers", {}).items()))
        elif kind == "askdetails":
            done("Answer payload:\n```json\n" + json.dumps(resp["updatedInput"].get("answers", {}), ensure_ascii=False) + "\n```")
        elif kind == "plan":
            done("The plan was approved; going on in " + mode + " mode.")
        else:
            out({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "tu-permit" + str(asks - 1), "content": "built"}]}})
            done("Ran it (" + ("for this session" if resp.get("updatedPermissions") else "once") + ").")
        continue
    if msg.get("type") != "user":
        continue
    content = msg["message"]["content"]
    text = next((b["text"] for b in content if b.get("type") == "text"), "")
    images = [b for b in content if b.get("type") == "image"]
    if images:
        # Says what it got, so a test can check the image arrived whole.
        text += " [images: " + ", ".join(b["source"]["media_type"] + " " + str(len(b["source"]["data"])) for b in images) + "]"
    out({"type": "user", "isReplay": True, "message": {"role": "user", "content": [{"type": "text", "text": text}]}})
    if slow:
        slow.set(); slow = None
        out({"type": "assistant", "message": {"id": "st", "content": [{"type": "text", "text": "steered: " + text}]}})
        out({"type": "result", "subtype": "success", "is_error": False, "result": "steered: " + text})
        continue
    if "CODEDETAILS" in text:
        code_details(); continue
    if "ASKDETAILS" in text:
        ask("askdetails", "AskUserQuestion", {"questions": DETAILS_QUESTIONS}, requires_user_interaction=True); continue
    if "SLOW" in text:
        slow = threading.Event()
        out({"type": "assistant", "message": {"id": "s", "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "sleep 600"}}]}})
        continue
    if "ASK" in text:
        ask("ask", "AskUserQuestion", {"questions": QUESTIONS}, requires_user_interaction=True); continue
    if "PLAN" in text:
        ask("plan", "ExitPlanMode", {"plan": "## Plan\n1. Use the month's real last day\n2. Run the tests"}, requires_user_interaction=True)
        continue
    if "PERMIT" in text:
        out({"type": "assistant", "message": {"id": "pm" + str(asks), "content": [{"type": "tool_use", "id": "tu-permit" + str(asks), "name": "Bash",
             "input": {"command": "make build"}}]}})
        ask("permit", "Bash", {"command": "make build"}, decision_reason="This command requires approval",
            permission_suggestions=[{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "make build"}],
                                     "behavior": "allow", "destination": "localSettings"}])
        continue
    if "RICH" in text:
        rich(); continue
    if "FAIL" in text:
        out({"type": "result", "subtype": "success", "is_error": True, "result": "API Error: 500 overloaded (fake)"}); continue
    if "EDITAT" in text:
        edit_at(); continue
    if "LATER" in text:
        # Answers on its own a few seconds later (LATER6: 6 s), whoever looks at the session meanwhile.
        secs = int("".join(c for c in text.split("LATER", 1)[1][:3] if c.isdigit()) or 8)
        threading.Timer(secs, done, ("later: " + text,)).start()
        continue
    if "FILES" in text:
        files(); continue
    if "AGENT" in text:
        subagent(); continue
    out({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "m"}}})
    for word in ("you ", "said: ", text):
        out({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": word}}})
    out({"type": "assistant", "message": {"id": "m", "content": [{"type": "text", "text": "you said: " + text}]}})
    out({"type": "result", "subtype": "success", "is_error": False, "result": "you said: " + text})
'''

for exe in (options.cli, options.app):
    if not exe.exists():
        sys.exit(f"missing {exe}: build it first (see the docstring)")
options.out.mkdir(parents=True, exist_ok=True)
for old in options.out.glob("*.png"):
    old.unlink()
# Never mistake a previous run's host audit for this run when startup fails before its log exists.
(options.out / "host-audit.jsonl").unlink(missing_ok=True)

with tempfile.TemporaryDirectory(prefix="adc-", dir=TMP, **({"ignore_cleanup_errors": True} if sys.version_info >= (3, 10) else {})) as tmp:
    base = Path(tmp)
    # Not inside `base`: on Windows, Python 3.12's mkdtemp gives it an owner-only ACL, and WebView2's
    # sandboxed processes then cannot use a data folder there (0x80080005 at startup).
    webview = Path(tempfile.gettempdir()) / f"adc-webview-{os.getpid()}"
    host_home, desk, work = base / "host", base / "desk", base / "work"
    work.mkdir()
    # Files for the @ menu.
    (work / "src").mkdir()
    (work / "src" / "main.rs").write_text("fn main() {}\n")
    (work / "export.py").write_text("def quarter_end(year, q):\n    return date(year, q * 3, 31)\n")
    if options.real:
        # A project command for the real agent's / menu (tools/desktop-chat-e2e-real.js picks it there).
        (work / ".claude" / "commands").mkdir(parents=True)
        (work / ".claude" / "commands" / "pong.md").write_text(
            "Reply with exactly the word SKILLPONG and nothing else.\n", encoding="utf-8")
    quiet = "[network]\nquiet = true\n"
    (host_home / "host").mkdir(parents=True)
    (host_home / "host" / "ai-remote.toml").write_text(
        '[host]\napproval_prompt = false\n\n' + quiet + '\n[[rule]]\ntool = "chat_*"\naction = "allow"\n', encoding="utf-8")
    for home in (host_home, desk):
        for role in ("host", "controller"):
            (home / role).mkdir(parents=True, exist_ok=True)
            if not (home / role / "ai-remote.toml").exists():
                (home / role / "ai-remote.toml").write_text(quiet, encoding="utf-8")
    agent = base / "rich_agent.py"
    agent.write_text(RICH_AGENT, encoding="utf-8")
    entry = f'command = {json.dumps(sys.executable)}\nargs = ["-u", {json.dumps(str(agent))}]\n'
    codex_entry = (f'command = {json.dumps(sys.executable)}\n'
                   f'args = ["-u", {json.dumps(str(ROOT / "tools" / "fake-codex-app-server.py"))}]\n')
    real_codex = os.environ.get("E2E_CODEX")
    extra = f'\n[[agent]]\nid = "codexbin"\nbase = "codex"\nname = "Codex (path)"\ncommand = {json.dumps(real_codex)}\n' if real_codex else ""
    (work / "README.md").write_text("# Report tool\n\nExports quarterly reports.\n", encoding="utf-8")
    (host_home / "host" / "chat.toml").write_text(
        f'[[agent]]\nid = "rich"\nbase = "claude"\nname = "Rich fake"\n{entry}\n'
        f'[[agent]]\nid = "nosteer"\nbase = "claude"\nname = "No-steer fake"\nsteer = false\n{entry}\n'
        f'[[agent]]\nid = "codexfake"\nbase = "codex"\nname = "Codex fake"\n{codex_entry}{extra}', encoding="utf-8")
    env = {k: v for k, v in os.environ.items() if k not in ("AI_REMOTE_HOST", "AI_REMOTE_HOME")}

    def cli(*args, home, timeout=120):
        return subprocess.run([str(options.cli), "--home", str(home), *map(str, args)], capture_output=True, text=True,
                              encoding="utf-8", errors="replace", timeout=timeout, env=env)

    log = open(base / "host.log", "w")
    stderr_log = open(options.out / "app.stderr.log", "w", encoding="utf-8")
    host = subprocess.Popen([str(options.cli), "--home", str(host_home), "host", "--no-pair-code"], stdout=log, stderr=log, env=env)
    app = None
    watchdog = None
    ci_layout = os.name == "nt" and os.environ.get("AI_REMOTE_CI_CAPTURE_LAYOUT") == "1"
    ci_native = None
    ci_shots = {}
    code = 1
    try:
        for _ in range(100):
            r = cli("--json", "pair-code", "--level", "admin", home=host_home)
            if r.returncode == 0:
                ticket = json.loads(r.stdout)["line"]
                break
            time.sleep(0.2)
        else:
            raise SystemExit("host did not start")
        doctor = cli("doctor", home=host_home).stdout
        network = next((l for l in doctor.splitlines() if l.split()[1:2] == ["network"]), "")
        assert network.split()[:1] == ["ok"] and "quiet" in network, "the test host is not in quiet mode:\n" + doctor
        # A new host name also makes each run's session references distinguishable in the logs.
        # The debug smoke window uses nonpersistent webview storage, separate from installed apps.
        assert cli("pair", ticket, "--name", f"e2e-{os.getpid()}", home=desk).returncode == 0, "the desktop profile did not pair"
        name = json.loads(cli("--json", "hosts", home=desk).stdout)[0]["name"]
        print(f"host {name!r}, desktop profile {desk}")

        script = base / "drive.js"
        source = ROOT / (os.environ.get("E2E_SCRIPT") or ("tools/desktop-chat-e2e-real.js" if options.real else "tools/desktop-chat-e2e.js"))
        metadata = '''(() => {
  const q = new URLSearchParams(location.search);
  window.__TAURI_INTERNALS__.invoke("log_from_page", { level: "e2e", message: "(context) " + JSON.stringify({
    visibility: document.visibilityState, focused: document.hasFocus(),
    reducedMotion: matchMedia("(prefers-reduced-motion: reduce)").matches,
    route: { path: location.pathname, view: q.get("view"), panel: q.get("panel"),
      hasHost: q.has("host"), hasSession: q.has("session") }
  }) });
})();
'''
        driver_source = source.read_text(encoding="utf-8")
        if ci_layout:
            driver_source = ci_driver_source(driver_source)
        script.write_text(metadata + SMOKE_INPUT_ISOLATION.replace("__ALLOW_NATIVE_INPUT__", json.dumps(options.visible))
                          + SMOKE_TRACE_UI + SMOKE_PREPARE_SHOT + (SMOKE_CI_CAPTURE_LAYOUT if ci_layout else "") + "\n" + driver_source
                          .replace("__CONFIG__", json.dumps({"work": str(work), "host": name, "agent": options.real,
                                                             "model": os.environ.get("E2E_MODEL")})), encoding="utf-8")
        app_env = {**env, "AI_REMOTE_HOME": str(desk), "AI_REMOTE_SMOKE": str(options.out / "zz-final.png"),
                   "AI_REMOTE_SMOKE_SECS": str(options.secs), "AI_REMOTE_SMOKE_SCRIPT": str(script),
                   # E2E_QUERY: another page to start on (and `shotlang` / `shottheme` to pin the look).
                   # The default chat suite begins with a genuinely missing workspace target, then
                   # uses its existing New action. Legacy rail preferences no longer select this UI.
                   "AI_REMOTE_SMOKE_QUERY": os.environ.get("E2E_QUERY") or (
                       f"view=agents&host={name}&session=missing-session-for-e2e"
                       if source.name == "desktop-chat-e2e.js" and not options.real
                       else f"view=session&host={name}&panel=chat"),
                   # Windows: keep the off-screen webview drawing, and a throwaway webview profile.
                   "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS": "--disable-features=CalculateNativeWinOcclusion",
                   "WEBVIEW2_USER_DATA_FOLDER": str(webview)}
        if options.visible:
            app_env["AI_REMOTE_SMOKE_VISIBLE"] = "1"
        app = subprocess.Popen([str(options.app)], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
                               encoding="utf-8", errors="replace", env=app_env)
        # A blocked UI thread can prevent the in-app smoke deadline from exiting. Bound the
        # external process too, so the harness reports a failure and can clean up its test homes.
        watchdog = threading.Timer(options.secs + 45, app.kill)
        watchdog.daemon = True
        watchdog.start()
        passed = None
        isolated_storage = False
        isolated_placement = options.visible
        for line in app.stderr:
            stderr_log.write(line)
            stderr_log.flush()
            line = line.rstrip()
            if line == "[smoke] isolated nonpersistent webview storage":
                isolated_storage = True
            if line.startswith("[smoke] geometry ") and not options.visible:
                try:
                    geometry = json.loads(line[len("[smoke] geometry "):])
                    isolated_placement = outside_displays(geometry)
                    if ci_layout:
                        from desktop_chat_ci_windows import client_box
                        ci_native = client_box(app.pid, geometry)
                except (ValueError, TypeError, AttributeError):
                    isolated_placement = False
                if not isolated_placement:
                    print("the smoke window is not verified outside every display")
                    app.terminate()
            if options.verbose:
                print("  |", line[:400])
            if line.startswith("[page e2e] "):
                text = line[len("[page e2e] "):]
                print(text)
                # A script can stop and start the host (tools/desktop-chat-offline-e2e.js: a dropped
                # connection, then the host back).
                if text.strip() == "(action) stop-host":
                    cli("stop-host", home=host_home)
                    try:
                        host.wait(timeout=20)
                    except subprocess.TimeoutExpired:
                        host.kill()
                elif text.startswith("(action) input "):
                    # Real key and mouse events (tools/win_input.py), e.g. to type through an input method.
                    import win_input
                    try:
                        print("     ", win_input.act(app.pid, json.loads(text[len("(action) input "):])))
                    except Exception as e:  # the page sees nothing happen and reports it
                        print("      input failed:", e)
                elif text.strip() == "(action) start-host":
                    host = subprocess.Popen([str(options.cli), "--home", str(host_home), "host", "--no-pair-code"],
                                            stdout=log, stderr=log, env=env)
                if text in ("all passed",) or re.fullmatch(r"\d+ failed", text):
                    passed = text == "all passed"
            elif ci_layout and line.startswith("[page ci-capture-layout] "):
                try:
                    layout = json.loads(line[len("[page ci-capture-layout] "):])
                    if isinstance(layout, dict) and isinstance(layout.get("name"), str):
                        ci_shots[layout["name"]] = layout
                except (ValueError, TypeError):
                    pass
            elif line.startswith("[page ") and ("error" in line or "warn" in line):
                print("  app:", line[:300])
            elif line.startswith("[smoke]"):
                print("  " + line)
        app_code = app.wait(timeout=60)
        print(f"  app exited with {app_code}")
        code = 0 if passed and app_code == 0 and isolated_storage and isolated_placement else 1
        if not isolated_placement:
            print("the default smoke run has no verified off-screen placement")
        if not isolated_storage:
            print("the debug app did not confirm isolated webview storage; rebuild it before running E2E")
        if passed is None:
            print("the script did not finish")
    finally:
        if watchdog is not None:
            watchdog.cancel()
        # A stopped script must not leave its test window alive (or writing its test profile).
        if app is not None and app.poll() is None:
            app.terminate()
            try:
                app.wait(timeout=10)
            except subprocess.TimeoutExpired:
                app.kill()
                app.wait(timeout=10)
        if app is not None:
            try:
                snapshot = cli("--json", "chat", "sessions", home=desk, timeout=10)
                (options.out / "sessions.json").write_text(json.dumps({"exit": snapshot.returncode,
                    "stdout": snapshot.stdout, "stderr": snapshot.stderr}, ensure_ascii=False, indent=2), encoding="utf-8")
            except subprocess.TimeoutExpired:
                (options.out / "sessions.json").write_text(json.dumps({"error": "session snapshot timed out"}), encoding="utf-8")
            exported = work / "export.py"
            if exported.is_file() and not exported.is_symlink() and exported.stat().st_size <= 1024 * 1024:
                shutil.copyfile(exported, options.out / "work-export.py")
        cli("stop-agent", home=desk)
        cli("stop-host", home=host_home)
        try:
            host.wait(timeout=10)
        except subprocess.TimeoutExpired:
            host.kill()
            host.wait(timeout=10)
        log.close()
        shutil.copyfile(base / "host.log", options.out / "host.log")
        # Only this harness-created --home is read, after its host has stopped and flushed the log.
        # Preserve the original JSONL, including failures and view_start/view_end, for replay checks.
        audit = host_home / "host" / "audit.jsonl"
        if audit.is_file() and not audit.is_symlink():
            shutil.copyfile(audit, options.out / "host-audit.jsonl")
        else:
            print("the isolated test host did not produce an audit.jsonl")
        stderr_log.close()
        if ci_layout:
            (options.out / "ci-layout.json").write_text(json.dumps({"native": ci_native, "shots": ci_shots}), encoding="utf-8")
        shutil.rmtree(webview, ignore_errors=True)
    if code:
        print((base / "host.log").read_text(encoding="utf-8", errors="replace")[-2000:])
    print(f"screenshots: {options.out}")
    sys.exit(code)
