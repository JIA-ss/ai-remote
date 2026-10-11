// Behavioral regressions for connection metadata and input buffered during takeover.
// Uses the installed desktop TypeScript compiler, no extra runtime/test dependency.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

function module(path, dependencies, timers, extra = "", globals = {}) {
  const source = readFileSync(new URL(path, import.meta.url), "utf8") + extra;
  const exports = {};
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText;
  vm.runInNewContext(compiled, {
    exports, require: (name) => {
      assert.ok(name in dependencies, `unexpected dependency ${name}`);
      return dependencies[name];
    }, Date, Map, Set, console, setTimeout: timers.set, clearTimeout: timers.clear,
    window: { setTimeout: timers.set, setInterval: timers.set }, clearInterval: timers.clear,
    ...globals,
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
    "../lib/i18n": { t: (key) => key },
    "../lib/manual-input": module("../apps/desktop/src/lib/manual-input.ts", {}, timers),
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

test("real zero-detail pointer releases click; drag and cancelled pointers do not duplicate clicks", async () => {
  const timers = clock();
  const calls = [];
  let refs = 0;
  const element = { focus() {}, getBoundingClientRect: () => ({ left: 10, top: 20, width: 100, height: 50 }) };
  const { RemoteInput } = module("../apps/desktop/src/ui/RemoteInput.tsx", {
    "../lib/i18n": { t: (key) => key },
    "../lib/manual-input": module("../apps/desktop/src/lib/manual-input.ts", {}, timers),
    react: { useRef: (current) => ({ current: refs++ === 0 ? element : current }), useEffect: (effect) => effect() },
    "../lib/api": { api: { callTool: async (...args) => { calls.push(args); } } },
    "react/jsx-runtime": { jsx: (_tag, props) => ({ props }) },
  }, timers);
  const { props } = RemoteInput({ host: "test-mac", screen: { x: 0, y: 0, width: 1000, height: 500 }, mac: true, onHandBack() {} });
  const pointer = (clientX, clientY) => ({ clientX, clientY, detail: 0, button: 0, pointerId: 1, currentTarget: { setPointerCapture() {} } });
  props.onPointerDown(pointer(60, 45));
  props.onPointerUp(pointer(60, 45));
  assert.equal(calls.length, 1);
  assert.equal(calls[0][2].action, "click");
  assert.equal(calls[0][2].x, 500);
  assert.equal(calls[0][2].y, 250);
  props.onPointerDown(pointer(60, 45));
  props.onPointerUp(pointer(80, 50));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(calls.length, 2);
  assert.equal(calls[1][2].action, "drag");
  props.onPointerDown(pointer(60, 45));
  props.onPointerCancel();
  props.onPointerUp(pointer(60, 45));
  assert.equal(calls.length, 2);
});

function videoFixture() {
  const timers = clock();
  const instances = [];
  const stats = [];
  let now = 0, requested = 0, drawn = 0;
  class MockDecoder {
    constructor(callbacks) { this.callbacks = callbacks; this.state = "unconfigured"; this.decodeQueueSize = 0; this.decoded = []; instances.push(this); }
    configure(config) { this.config = config; this.state = "configured"; }
    decode(chunk) { this.decoded.push(chunk); }
    close() { this.state = "closed"; }
    output(width = 1920, height = 1080) {
      let closed = false;
      this.callbacks.output({ displayWidth: width, displayHeight: height, close: () => { closed = true; } });
      assert.ok(closed, "every decoded frame must be released");
    }
  }
  const exports = module("../apps/desktop/src/lib/video.ts", { "../bindings": { commands: {} } }, timers, "", {
    performance: { now: () => now }, VideoDecoder: MockDecoder, EncodedVideoChunk: class { constructor(data) { Object.assign(this, data); } },
  });
  const canvas = { width: 0, height: 0, getContext: () => ({ drawImage() { drawn++; } }) };
  const decoder = exports.createDecoder(canvas, () => { requested++; }, () => {}, (value) => stats.push(value));
  const packet = (key = false) => {
    const payload = new Uint8Array([0, 0, 0, 1, key ? 0x67 : 0x61, 0x42, 0, 0x28]);
    const buffer = new ArrayBuffer(18 + payload.length);
    const head = new DataView(buffer);
    head.setUint8(0, 1); head.setUint8(1, key ? 1 : 0); head.setUint16(2, 1920); head.setUint16(4, 1080); head.setUint32(14, payload.length);
    new Uint8Array(buffer, 18).set(payload);
    return buffer;
  };
  return { ...exports, decoder, instances, stats, timers, packet, advance: (n) => { now += n; }, requested: () => requested, drawn: () => drawn };
}

test("video packets reject truncated payloads and telemetry counts displayed frames, not requests", async () => {
  const s = videoFixture();
  assert.equal(s.parseFrame(s.packet(true).slice(0, 20)), null);
  s.decoder.push(s.packet(true));
  s.instances[0].output();
  s.decoder.push(s.packet());
  s.instances[0].output();
  s.advance(1000);
  await s.timers.run(1000);
  assert.equal(s.stats[0].width, 1920);
  assert.equal(s.stats[0].decodedFps, 2);
  assert.equal(s.stats[0].receivedKbps, 2 * 26 * 8 / 1000);
  s.decoder.close();
});

test("decoder backlog is discarded, recovery requests bounded, and stale output is released", () => {
  const s = videoFixture();
  s.decoder.push(s.packet(true));
  const retired = s.instances[0];
  retired.decodeQueueSize = 2;
  s.decoder.push(s.packet());
  assert.equal(retired.state, "closed");
  assert.equal(s.requested(), 1);
  s.decoder.push(s.packet());
  assert.equal(s.requested(), 1);
  s.advance(300);
  s.decoder.push(s.packet());
  assert.equal(s.requested(), 2);
  s.decoder.push(s.packet(true));
  retired.output();
  assert.equal(s.drawn(), 0, "old decoder output cannot paint after recovery");
  s.instances[1].output();
  assert.equal(s.drawn(), 1);
  retired.callbacks.error(new Error("late retired decoder error"));
  assert.equal(s.instances[1].state, "configured");
  s.decoder.close();
  s.instances[1].output();
  assert.equal(s.drawn(), 1);
  assert.equal(s.timers.pending.size, 0);
});

test("back-to-back keyframes cannot bypass the decoder queue bound", () => {
  const s = videoFixture();
  s.decoder.push(s.packet(true));
  for (let i = 0; i < 5; i++) {
    const previous = s.instances.at(-1);
    previous.decodeQueueSize = 2;
    s.decoder.push(s.packet(true));
    assert.equal(previous.state, "closed");
    assert.equal(s.instances.at(-1).decoded.length, 1);
  }
  assert.equal(s.requested(), 0, "the arriving IDR is already sufficient for recovery");
  s.decoder.close();
});
