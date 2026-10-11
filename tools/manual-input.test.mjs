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

function remoteInputFixture() {
  const calls = [], errors = [], cleanups = [], pendingTimers = new Map();
  const first = deferred();
  let timerId = 0, refId = 0, handBack = 0;
  const element = { focus() {}, getBoundingClientRect: () => ({ left: 0, top: 0, width: 100, height: 100 }) };
  const setTimer = (fn) => { const id = ++timerId; pendingTimers.set(id, fn); return id; };
  const clearTimer = (id) => pendingTimers.delete(id);
  const source = readFileSync(new URL("../apps/desktop/src/ui/RemoteInput.tsx", import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText;
  const exports = {};
  const dependencies = {
    react: { useRef: (value) => ({ current: refId++ === 0 ? element : value }), useEffect: (effect) => cleanups.push(effect()) },
    "react/jsx-runtime": { jsx: (_tag, props) => ({ props }) },
    "../lib/i18n": { t: (key, vars) => `${key}${vars?.error ? `: ${vars.error}` : ""}` },
    "../lib/manual-input": { createManualInputDispatcher },
    "../lib/api": { api: { callTool: (host, tool, args, person) => {
      calls.push({ host, tool, args, person });
      return calls.length === 1 ? first.promise : Promise.resolve({});
    } } },
  };
  vm.runInNewContext(compiled, { exports, require: (name) => {
    assert.ok(name in dependencies, `unexpected dependency ${name}`);
    return dependencies[name];
  }, window: { setTimeout: setTimer }, clearTimeout: clearTimer, Date, console });
  const { props } = exports.RemoteInput({ host: "test-mac", screen: { x: 0, y: 0, width: 1000, height: 1000 }, mac: true,
    onError: (message) => { if (message) errors.push(message); },
    onHandBack: () => { assert.equal(pendingTimers.size, 0, "cancel text BEFORE handing back"); handBack++; },
  });
  const key = (value, options = {}) => ({ key: value, ctrlKey: false, altKey: false, metaKey: false, shiftKey: false, preventDefault() {}, ...options });
  const pointer = { clientX: 50, clientY: 50, detail: 0, button: 0, pointerId: 1, currentTarget: { setPointerCapture() {} } };
  return { calls, errors, cleanups, pendingTimers, first, props, key, pointer, handBack: () => handBack };
}

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
