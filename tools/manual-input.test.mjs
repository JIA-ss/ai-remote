// Manual input must stay ordered, bounded, and cancellable without replaying a partial call.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

const source = readFileSync(new URL("../apps/desktop/src/lib/manual-input.ts", import.meta.url), "utf8");
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
const exports = {};
vm.runInNewContext(compiled, { exports, Error, console });
const { createManualInputDispatcher } = exports;
const tick = () => new Promise((resolve) => setImmediate(resolve));
const plain = (value) => JSON.parse(JSON.stringify(value));
function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test("rapid actions execute in order with exactly one call in flight", async () => {
  const calls = [], pending = [], errors = [];
  let active = 0, maximum = 0;
  const input = createManualInputDispatcher((tool, args) => {
    calls.push({ tool, args });
    const next = deferred();
    pending.push(next);
    maximum = Math.max(maximum, ++active);
    return next.promise.finally(() => active--);
  }, (error) => errors.push(error));
  input.enqueue("mouse", { action: "click", x: 1 });
  input.enqueue("keyboard", { keys: "enter" });
  input.enqueue("mouse", { action: "click", x: 2 });
  assert.equal(calls.length, 1);
  pending[0].resolve({}); await tick();
  assert.equal(calls.length, 2);
  pending[1].resolve({}); await tick();
  assert.equal(calls.length, 3);
  pending[2].resolve({}); await tick();
  assert.equal(maximum, 1);
  assert.equal(errors.length, 0);
  assert.deepEqual(plain(calls).map((call) => call.args), [{ action: "click", x: 1 }, { keys: "enter" }, { action: "click", x: 2 }]);
});

test("only adjacent queued text coalesces, preserving keyboard and mouse barriers", async () => {
  const first = deferred(), calls = [];
  const input = createManualInputDispatcher((tool, args) => {
    calls.push({ tool, args });
    return calls.length === 1 ? first.promise : Promise.resolve({});
  }, () => assert.fail("unexpected error"));
  input.enqueue("mouse", { action: "click" });
  input.enqueue("keyboard", { text: "a" });
  input.enqueue("keyboard", { text: "b" });
  input.enqueue("keyboard", { keys: "enter" });
  input.enqueue("keyboard", { text: "c" });
  input.enqueue("keyboard", { text: "d" });
  first.resolve({}); await tick();
  assert.deepEqual(plain(calls).map((call) => call.args), [{ action: "click" }, { text: "ab" }, { keys: "enter" }, { text: "cd" }]);
});

test("a failed or partially sent call clears all pending input and is never retried", async () => {
  const first = deferred(), calls = [], errors = [];
  const input = createManualInputDispatcher((tool, args) => {
    calls.push({ tool, args });
    return first.promise;
  }, (error) => errors.push(error));
  input.enqueue("keyboard", { text: "first" });
  input.enqueue("keyboard", { keys: "enter" });
  first.reject(new Error("input interrupted after partial delivery")); await tick();
  assert.equal(calls.length, 1);
  assert.equal(errors.length, 1);
  assert.equal(errors[0].reason, "tool");
  assert.match(errors[0].message, /partial delivery/);
  await tick();
  assert.equal(calls.length, 1);
});

test("resolved tool error results also stop the queue without a retry", async () => {
  const first = deferred(), calls = [], errors = [];
  const input = createManualInputDispatcher((tool) => { calls.push(tool); return first.promise; }, (error) => errors.push(error));
  input.enqueue("mouse", { action: "click" });
  input.enqueue("keyboard", { keys: "enter" });
  first.resolve({ isError: true, content: [{ type: "text", text: "input busy" }] }); await tick();
  assert.deepEqual(calls, ["mouse"]);
  assert.equal(errors[0].message, "input busy");
});

test("cancel discards unsent actions and ignores a retired failure while preserving one in flight", async () => {
  const first = deferred(), calls = [], errors = [];
  const input = createManualInputDispatcher((tool, args) => {
    calls.push({ tool, args });
    return calls.length === 1 ? first.promise : Promise.resolve({});
  }, (error) => errors.push(error));
  input.enqueue("keyboard", { text: "already sent" });
  input.enqueue("keyboard", { text: "unsent" });
  input.cancel();
  input.enqueue("keyboard", { text: "new generation" });
  assert.equal(calls.length, 1);
  first.reject(new Error("old takeover ended")); await tick();
  assert.deepEqual(plain(calls).map((call) => call.args.text), ["already sent", "new generation"]);
  assert.equal(errors.length, 0);
});

test("queue overflow clears unsent actions and reports a visible failure", async () => {
  const first = deferred(), calls = [], errors = [];
  const input = createManualInputDispatcher((tool, args) => { calls.push({ tool, args }); return first.promise; }, (error) => errors.push(error));
  input.enqueue("mouse", { action: "click", x: 0 });
  for (let x = 1; x <= 32; x++) assert.equal(input.enqueue("mouse", { action: "click", x }), true);
  assert.equal(input.enqueue("mouse", { action: "click", x: 33 }), false);
  assert.equal(errors[0].reason, "overflow");
  first.resolve({}); await tick();
  assert.equal(calls.length, 1);
});

test("text coalescing remains bounded below the host's text limit", async () => {
  const first = deferred(), calls = [];
  const input = createManualInputDispatcher((tool, args) => { calls.push({ tool, args }); return calls.length === 1 ? first.promise : Promise.resolve({}); }, () => assert.fail("unexpected error"));
  input.enqueue("mouse", { action: "click" });
  input.enqueue("keyboard", { text: "x".repeat(1024) });
  input.enqueue("keyboard", { text: "y" });
  first.resolve({}); await tick();
  assert.deepEqual(plain(calls).slice(1).map((call) => call.args.text.length), [1024, 1]);
});

const liveInputSource = readFileSync(new URL("../apps/desktop/src/lib/live-input.ts", import.meta.url), "utf8");
const liveInputCompiled = ts.transpileModule(liveInputSource, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
const liveInput = {};
vm.runInNewContext(liveInputCompiled, { exports: liveInput, Error, console, JSON, Math, Set, setTimeout, clearTimeout });

/** `live`: the host offers the live input channel (its lines go to `sent`); otherwise the fallback runs. */
function remoteInputFixture(live = false, screen = { x: 0, y: 0, width: 1000, height: 1000 }) {
  const calls = [], errors = [], cleanups = [], pendingTimers = new Map(), sent = [], notices = [];
  const first = deferred();
  let timerId = 0, refId = 0, handBack = 0, closed = 0;
  const element = { focus() {}, getBoundingClientRect: () => ({ left: 0, top: 0, width: 100, height: 100 }) };
  const setTimer = (fn) => { const id = ++timerId; pendingTimers.set(id, fn); return id; };
  const clearTimer = (id) => pendingTimers.delete(id);
  const source = readFileSync(new URL("../apps/desktop/src/ui/RemoteInput.tsx", import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText;
  const exports = {};
  const opened = deferred();
  const dependencies = {
    react: {
      useRef: (value) => ({ current: refId++ === 0 ? element : value }),
      useEffect: (effect) => cleanups.push(effect()),
      useState: (value) => [value, () => {}],
    },
    "react/jsx-runtime": { jsx: (_tag, props) => ({ props }) },
    "../lib/i18n": { t: (key, vars) => `${key}${vars?.error ? `: ${vars.error}` : ""}` },
    "../lib/manual-input": { createManualInputDispatcher },
    "../lib/live-input": liveInput,
    "../lib/platform": { detectOs: () => "mac" },
    "../lib/api": { api: {
      callTool: (host, tool, args, person) => {
        calls.push({ host, tool, args, person });
        return calls.length === 1 ? first.promise : Promise.resolve({});
      },
      openInput: () => opened.promise,
      clipboardText: async () => null,
      clipboardFiles: async () => [],
      setClipboardText: async () => {},
    } },
  };
  vm.runInNewContext(compiled, { exports, require: (name) => {
    assert.ok(name in dependencies, `unexpected dependency ${name}`);
    return dependencies[name];
  }, window: { setTimeout: setTimer, setInterval: () => 0 }, clearTimeout: clearTimer, clearInterval() {}, setTimeout, Date, console, JSON,
    document: { hasFocus: () => false } });
  if (live) opened.resolve({ os: "windows", clipboard: false, clipboardWrite: false, send: async (data) => { sent.push(data); return true; }, close: () => closed++ });
  else opened.reject(new Error("older host"));
  const { props } = exports.RemoteInput({ host: "test-mac", screen, mac: !live,
    onError: (message) => { if (message) errors.push(message); },
    onNotice: (message) => notices.push(message),
    onHandBack: () => { assert.equal(pendingTimers.size, 0, "cancel text BEFORE handing back"); handBack++; },
  });
  const key = (value, options = {}) => ({ key: value, ctrlKey: false, altKey: false, metaKey: false, shiftKey: false, preventDefault() {}, ...options });
  const pointer = { clientX: 50, clientY: 50, detail: 0, button: 0, pointerId: 1, currentTarget: { setPointerCapture() {} } };
  const lines = () => sent.flatMap((d) => d.trimEnd().split("\n")).map((l) => JSON.parse(l));
  return { calls, errors, notices, cleanups, pendingTimers, first, props, key, pointer, handBack: () => handBack, sent, lines, closed: () => closed };
}

test("with the live channel, moves, presses, wheel and keys stream as lines and nothing becomes a tool call", async () => {
  const s = remoteInputFixture(true);
  await tick();
  const wait = () => new Promise((resolve) => setTimeout(resolve, 10));
  s.props.onPointerMove({ clientX: 10, clientY: 20 });
  s.props.onPointerDown({ ...s.pointer, clientX: 10, clientY: 20 });
  s.props.onPointerMove({ clientX: 60, clientY: 20 });
  s.props.onPointerUp({ ...s.pointer, clientX: 60, clientY: 20 });
  await wait();
  assert.deepEqual(s.lines(), [
    { m: [100, 200] }, { b: "left", down: true }, { m: [599, 200] }, { b: "left", down: false },
  ]);
  s.props.onWheel({ clientX: 60, clientY: 20, deltaX: 0, deltaY: 100, deltaMode: 0 });
  s.props.onKeyDown(s.key("Meta", { code: "MetaLeft", metaKey: true }));
  s.props.onKeyDown(s.key("c", { code: "KeyC", metaKey: true }));
  s.props.onKeyUp(s.key("Meta", { code: "MetaLeft" }));
  await wait();
  // Cmd on this Mac drives a Windows host as Ctrl; the key held under it is let go with it.
  assert.deepEqual(s.lines().slice(4), [
    { m: [599, 200] }, { w: [0, 2] }, { k: "ctrl", down: true }, { k: "c", down: true }, { k: "ctrl", down: false }, { k: "c", down: false },
  ]);
  // A second button pressed while the first is held arrives as a pointermove with both bits set.
  s.props.onPointerDown({ ...s.pointer, clientX: 10, clientY: 20, buttons: 1 });
  s.props.onPointerMove({ clientX: 10, clientY: 20, buttons: 3 });
  s.props.onPointerMove({ clientX: 10, clientY: 20, buttons: 1 });
  s.props.onPointerUp({ ...s.pointer, clientX: 10, clientY: 20, buttons: 0 });
  await wait();
  assert.deepEqual(s.lines().slice(10).filter((l) => "b" in l), [
    { b: "left", down: true }, { b: "right", down: true }, { b: "right", down: false }, { b: "left", down: false },
  ]);
  // ⌃⌘F is the app's full-screen toggle: the F never reaches the host.
  const before = s.lines().length;
  s.props.onKeyDown(s.key("f", { code: "KeyF", ctrlKey: true, metaKey: true }));
  await wait();
  assert.equal(s.lines().length, before);
  s.props.onKeyDown(s.key("h", { ctrlKey: true, altKey: true }));
  assert.equal(s.handBack(), 1);
  assert.equal(s.calls.length, 0, "no tool calls on the live path");
  for (const cleanup of s.cleanups) cleanup();
  assert.equal(s.closed(), 1);
});

test("input lands on the streamed monitor: one left of the primary (negative origin) and portrait", async () => {
  // The picture is 100x100 CSS pixels here; the monitor is 1440x2560 at (-1440, -9), as on win-dev.
  const left = { x: -1440, y: -9, width: 1440, height: 2560 };
  const s = remoteInputFixture(true, left);
  await tick();
  const wait = () => new Promise((resolve) => setTimeout(resolve, 10));
  // Each move waits for the last to go out: runs of moves collapse to the newest.
  for (const [clientX, clientY] of [[0, 0], [100, 100], [50, 25]]) {
    s.props.onPointerMove({ clientX, clientY });
    await wait();
  }
  // The corners are the monitor's first and last pixels; nothing reaches the primary monitor at x >= 0.
  assert.deepEqual(s.lines(), [{ m: [-1440, -9] }, { m: [-1, 2550] }, { m: [-720, 631] }]);
  // A wheel notch moves there first, so it scrolls what is under the pointer on this monitor.
  s.props.onWheel({ clientX: 10, clientY: 90, deltaX: 0, deltaY: 100, deltaMode: 0 });
  await wait();
  assert.deepEqual(s.lines().slice(3), [{ m: [-1296, 2294] }, { w: [0, 2] }]);
  for (const cleanup of s.cleanups) cleanup();
  // Without the live channel, the tool calls take the same screen coordinates.
  const right = remoteInputFixture(false, { x: 5120, y: -9, width: 1440, height: 2560 });
  await tick();
  right.props.onPointerDown({ ...right.pointer, clientX: 100, clientY: 0 });
  right.props.onPointerUp({ ...right.pointer, clientX: 100, clientY: 0 });
  assert.deepEqual(plain(right.calls[0].args), { action: "click", x: 6559, y: -9, space: "screen" });
});

test("without the live channel the fallback is used and a note says so", async () => {
  const s = remoteInputFixture(false);
  await tick();
  assert.match(s.notices[0], /live_input_unavailable/);
  s.props.onPointerDown(s.pointer); s.props.onPointerUp(s.pointer);
  assert.equal(s.calls.length, 1);
  assert.equal(s.calls[0].tool, "mouse");
});

test("hand-back hotkey cancels queued calls, buffered text and gestures before notifying the host", async () => {
  const s = remoteInputFixture();
  s.props.onKeyDown(s.key("Enter"));
  s.props.onPointerDown(s.pointer); s.props.onPointerUp(s.pointer);
  s.props.onPointerDown(s.pointer);
  s.props.onKeyDown(s.key("a"));
  assert.equal(s.pendingTimers.size, 1);
  s.props.onKeyDown(s.key("h", { ctrlKey: true, altKey: true }));
  assert.equal(s.handBack(), 1);
  s.first.resolve({}); await tick();
  s.props.onPointerUp(s.pointer);
  assert.equal(s.calls.length, 1);
});

test("blur, pointer cancel, geometry reset and unmount discard unsent input", async () => {
  for (const cancel of ["blur", "pointer", "geometry", "unmount"]) {
    const s = remoteInputFixture();
    s.props.onKeyDown(s.key("Enter"));
    s.props.onPointerDown(s.pointer); s.props.onPointerUp(s.pointer);
    s.props.onPointerDown(s.pointer);
    s.props.onKeyDown(s.key("a"));
    if (cancel === "blur") s.props.onBlur();
    if (cancel === "pointer") s.props.onPointerCancel();
    if (cancel === "geometry") s.cleanups[1]();
    if (cancel === "unmount") for (const cleanup of s.cleanups) cleanup();
    assert.equal(s.pendingTimers.size, 0, cancel);
    s.first.resolve({}); await tick();
    s.props.onPointerUp(s.pointer);
    assert.equal(s.calls.length, 1, cancel);
  }
});

test("a tool failure also clears text not yet enqueued and reports the error in the session", async () => {
  const s = remoteInputFixture();
  s.props.onKeyDown(s.key("Enter"));
  s.props.onPointerDown(s.pointer); s.props.onPointerUp(s.pointer);
  s.props.onPointerDown(s.pointer);
  s.props.onKeyDown(s.key("a"));
  s.first.resolve({ isError: true, content: [{ type: "text", text: "target locked during input" }] }); await tick();
  assert.equal(s.pendingTimers.size, 0);
  s.props.onPointerUp(s.pointer);
  assert.equal(s.calls.length, 1);
  assert.match(s.errors[0], /target locked/);
});
