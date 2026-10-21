// Live input must batch, collapse movement, turn wheel pixels into notches and name keys the host understands.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

const source = readFileSync(new URL("../apps/desktop/src/lib/live-input.ts", import.meta.url), "utf8");
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
const exports = {};
vm.runInNewContext(compiled, { exports, Error, console, JSON, Math, Set });
const { createLiveInput, keyNameOf } = exports;

/** A stream whose timer fires only when `fire()` is called. */
function harness(options = {}) {
  const sent = [];
  let pending = null;
  const input = createLiveInput((data) => sent.push(data), {
    ...options,
    schedule: (fn) => { pending = fn; return 1; },
    cancel: () => { pending = null; },
  });
  const lines = () => sent.flatMap((d) => d.trimEnd().split("\n")).map((l) => JSON.parse(l));
  return { input, sent, lines, fire: () => { const f = pending; pending = null; f?.(); } };
}

test("moves collapse to the last one in a batch, presses keep their order", () => {
  const h = harness();
  h.input.move(1, 1);
  h.input.move(2, 2);
  h.input.move(3.4, 3.6);
  h.input.button("left", true);
  h.input.move(5, 5);
  h.input.move(6, 6);
  h.input.button("left", false);
  assert.equal(h.sent.length, 0, "nothing goes before the timer");
  h.fire();
  assert.equal(h.sent.length, 1, "one write per batch");
  assert.deepEqual(h.lines(), [{ m: [3, 4] }, { b: "left", down: true }, { m: [6, 6] }, { b: "left", down: false }]);
});

test("wheel pixels accumulate into whole notches with the remainder kept", () => {
  const h = harness({ pxPerNotch: 50 });
  h.input.wheel(0, 30, 0);
  h.fire();
  assert.equal(h.sent.length, 0, "30 px is less than a notch");
  h.input.wheel(0, 30, 0);
  h.fire();
  assert.deepEqual(h.lines(), [{ w: [0, 1] }]);
  h.input.wheel(-3, 0, 1);
  h.fire();
  assert.deepEqual(h.lines().at(-1), { w: [-1, 0] });
  h.input.wheel(0, 100000, 0);
  h.fire();
  assert.deepEqual(h.lines().at(-1), { w: [0, 50] }, "clamped to what the host accepts");
});

test("close releases what is held and sends nothing afterwards", () => {
  const h = harness();
  h.input.key("ctrl", true);
  h.input.key("c", true);
  assert.deepEqual([...h.input.held], ["ctrl", "c"]);
  h.input.close();
  assert.deepEqual(h.lines(), [{ k: "ctrl", down: true }, { k: "c", down: true }, { release: true }]);
  h.input.move(9, 9);
  h.fire();
  assert.equal(h.lines().length, 3);
});

test("keys get the host's names and the command key crosses platforms", () => {
  assert.equal(keyNameOf({ key: "Enter" }, "macos", "windows"), "enter");
  assert.equal(keyNameOf({ key: "ArrowLeft" }, "macos", "windows"), "left");
  assert.equal(keyNameOf({ key: "F11" }, "macos", "windows"), "f11");
  assert.equal(keyNameOf({ key: "A" }, "macos", "windows"), "A");
  assert.equal(keyNameOf({ key: "Dead" }, "macos", "windows"), null);
  assert.equal(keyNameOf({ key: "Process" }, "macos", "windows"), null);
  assert.equal(keyNameOf({ key: "Meta" }, "macos", "windows"), "ctrl", "Cmd on a Mac is Ctrl on Windows");
  assert.equal(keyNameOf({ key: "Meta" }, "macos", "macos"), "meta");
  assert.equal(keyNameOf({ key: "Control" }, "windows", "macos"), "meta", "Ctrl on a PC is Cmd on a Mac");
  assert.equal(keyNameOf({ key: "Control" }, "macos", "windows"), "ctrl");
  assert.equal(keyNameOf({ key: "AltGraph" }, "windows", "windows"), null, "AltGr itself is not sent; the character it makes is");
});

test("with a shortcut modifier, letters and digits are named by key position, not by the local layout", () => {
  assert.equal(keyNameOf({ key: "с", code: "KeyC", ctrlKey: true }, "windows", "windows"), "c", "Ctrl+C on a Russian layout");
  assert.equal(keyNameOf({ key: "å", code: "KeyA", altKey: true }, "macos", "macos"), "a", "Option+A on a Mac");
  assert.equal(keyNameOf({ key: "!", code: "Digit1", metaKey: true }, "macos", "windows"), "1");
  assert.equal(keyNameOf({ key: "!", code: "Digit1", shiftKey: true }, "macos", "windows"), "!", "shift alone keeps the character");
});

test("a Chromium mouse wheel notch (100 px) is one notch by default, and heartbeats are plain lines", () => {
  const h = harness();
  h.input.wheel(0, 100, 0);
  h.input.heartbeat();
  h.fire();
  assert.deepEqual(h.lines(), [{ w: [0, 1] }, { hb: 1 }]);
});
