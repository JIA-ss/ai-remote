// Behavioral regressions for connection metadata and input buffered during takeover.
// Uses the installed desktop TypeScript compiler, no extra runtime/test dependency.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

function module(path, dependencies, timers, extra = "") {
  const source = readFileSync(new URL(path, import.meta.url), "utf8") + extra;
  const exports = {};
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText;
  vm.runInNewContext(compiled, {
    exports, require: (name) => {
      assert.ok(name in dependencies, `unexpected dependency ${name}`);
      return dependencies[name];
    }, Date, Map, Set, console, setTimeout: timers.set, clearTimeout: timers.clear,
    window: { setTimeout: timers.set },
  }, { filename: path });
  return exports;
}

function clock() {
  const pending = new Map();
  let next = 1;
  return {
    pending,
    set(fn, delay) { const id = next++; pending.set(id, { fn, delay }); return id; },
    clear(id) { pending.delete(id); },
    async run(delay) {
      const tasks = [...pending].filter(([, item]) => item.delay === delay);
      for (const [id, item] of tasks) { pending.delete(id); await item.fn(); }
    },
  };
}

function setupStore() {
  const timers = clock();
  const feed = module("../apps/desktop/src/lib/feed.ts", {}, timers);
  let host = { name: "test-mac", device: "test", levelAtPairing: "control", pairedAt: 1, path: null };
  let listener;
  const api = {
    hosts: async () => [{ ...host }],
    subscribe: async (_name, callback) => { listener = callback; callback({ kind: "hello", host: host.name, version: "test", os: "macos", level: "control" }); return () => {}; },
  };
  const { Store } = module("../apps/desktop/src/lib/store.ts", { react: {}, "./api": { api }, "./feed": feed }, timers, "\nexport { Store };");
  return { store: new Store(), timers, api, feed, setHost: (next) => { host = { ...host, ...next }; }, event: (event) => listener(event) };
}

test("cold connections query the actual path after hello; reconnect clears an old route", async () => {
  const s = setupStore();
  await s.store.start();
  assert.equal(s.store.get().computers[0].online, true);
  assert.equal(s.store.get().computers[0].host.path, null);
  s.setHost({ path: "direct" });
  await s.timers.run(250);
  assert.equal(s.store.get().computers[0].host.path, "direct");
  s.event({ kind: "closed" });
  assert.equal(s.store.get().computers[0].online, false);
  assert.equal(s.store.get().computers[0].host.path, null);
  s.event({ kind: "hello", host: "test-mac", version: "test", level: "observe" });
  s.setHost({ path: "relay" });
  await s.timers.run(250);
  assert.equal(s.store.get().computers[0].host.path, "relay");
  assert.equal(s.store.get().computers[0].feed.level, "observe");
});

test("refresh replaces host metadata without discarding active approvals or driver", async () => {
  const s = setupStore();
  await s.store.start();
  s.event({ kind: "driver", state: "person", by: "owner", ts: 1, agent_may_watch: false });
  const previous = s.store.get().computers[0].feed;
  s.setHost({ path: "relay", levelAtPairing: "observe" });
  await s.store.refresh();
  const computer = s.store.get().computers[0];
  assert.equal(computer.host.path, "relay");
  assert.equal(computer.host.levelAtPairing, "observe");
  assert.equal(computer.feed, previous);
  assert.equal(computer.feed.driver.state, "person");
});

test("stale metadata responses cannot overwrite a newer reconnect", async () => {
  const s = setupStore();
  await s.store.start();
  let resolve;
  s.api.hosts = () => new Promise((done) => { resolve = done; });
  const first = s.timers.run(250);
  s.event({ kind: "hello", host: "test-mac", version: "new", level: "control" });
  resolve([{ name: "test-mac", path: "direct" }]);
  await first;
  assert.equal(s.store.get().computers[0].host.path, null);
  s.api.hosts = async () => [{ name: "test-mac", path: "relay" }];
  await s.timers.run(250);
  assert.equal(s.store.get().computers[0].host.path, "relay");
});

test("buffered input is discarded on leaving takeover; live buffered text still sends", async () => {
  const timers = clock();
  const calls = [];
  const cleanups = [];
  const { RemoteInput } = module("../apps/desktop/src/ui/RemoteInput.tsx", {
    react: { useRef: (current) => ({ current }), useEffect: (effect) => cleanups.push(effect()) },
    "../lib/api": { api: { callTool: async (...args) => { calls.push(args); } } },
    "react/jsx-runtime": { jsx: (_tag, props) => ({ props }) },
  }, timers);
  const props = { host: "test-mac", screen: null, mac: true, onHandBack: () => {} };
  const key = (value) => ({ key: value, ctrlKey: false, altKey: false, metaKey: false, shiftKey: false, preventDefault() {} });
  RemoteInput(props).props.onKeyDown(key("a"));
  cleanups.pop()();
  await timers.run(60);
  assert.equal(calls.length, 0, "no input may outlive the removed control layer");
  const second = RemoteInput(props);
  second.props.onKeyDown(key("a"));
  second.props.onKeyDown(key("b"));
  await timers.run(60);
  assert.equal(calls.length, 1);
  assert.equal(calls[0][2].text, "ab");
  cleanups.pop()();
});
