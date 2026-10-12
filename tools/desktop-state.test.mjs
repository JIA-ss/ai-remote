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

test("each hello says whether the host bypasses this device; the level stays the real one", async () => {
  const s = setupStore();
  await s.store.start();
  assert.equal(s.store.get().computers[0].feed.bypass, false);
  s.event({ kind: "hello", host: "test-mac", version: "test", level: "observe", bypass: true });
  assert.equal(s.store.get().computers[0].feed.bypass, true);
  assert.equal(s.store.get().computers[0].feed.level, "observe");
  // Hosts from before bypass do not send the field: not bypassed.
  s.event({ kind: "hello", host: "test-mac", version: "old", level: "observe" });
  assert.equal(s.store.get().computers[0].feed.bypass, false);
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
  const codec = module("../apps/desktop/src/lib/codec.ts", {}, timers);
  const exports = module("../apps/desktop/src/lib/video.ts", { "../bindings": { commands: {} }, "./codec": codec }, timers, "", {
    performance: { now: () => now }, VideoDecoder: MockDecoder, EncodedVideoChunk: class { constructor(data) { Object.assign(this, data); } },
  });
  const canvas = { width: 0, height: 0, getContext: () => ({ drawImage() { drawn++; } }) };
  const decoder = exports.createDecoder(canvas, () => { requested++; }, () => {}, (value) => stats.push(value));
  // Frames 1/60 s apart in stream time, as a 60 fps host sends them.
  let ts = 0n;
  const packet = (key = false) => {
    const payload = new Uint8Array([0, 0, 0, 1, key ? 0x67 : 0x61, 0x42, 0, 0x28]);
    const buffer = new ArrayBuffer(18 + payload.length);
    const head = new DataView(buffer);
    head.setUint8(0, 1); head.setUint8(1, key ? 1 : 0); head.setUint16(2, 1920); head.setUint16(4, 1080); head.setUint32(14, payload.length);
    head.setBigUint64(6, ts); ts += 16_667n;
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

// At 60 fps, 150 ms of stream time is 9 queued frames.
const BEHIND = 9;
// Frames the current decoder has decoded without falling behind, so a queue can be that long.
const fill = (s, n = BEHIND) => { for (let i = 0; i < n; i++) s.decoder.push(s.packet()); };

test("decoder backlog is discarded, recovery requests bounded, and stale output is released", () => {
  const s = videoFixture();
  s.decoder.push(s.packet(true));
  const retired = s.instances[0];
  retired.output();
  fill(s);
  const shown = s.drawn();
  retired.decodeQueueSize = BEHIND;
  s.decoder.push(s.packet());
  assert.equal(retired.state, "configured", "a lag is ridden out at first");
  s.advance(300);
  s.decoder.push(s.packet());
  assert.equal(retired.state, "closed", "a lag that lasts drops to the next keyframe");
  assert.equal(s.requested(), 1);
  s.decoder.push(s.packet());
  assert.equal(s.requested(), 1);
  s.advance(300);
  s.decoder.push(s.packet());
  assert.equal(s.requested(), 2);
  s.decoder.push(s.packet(true));
  retired.output();
  assert.equal(s.drawn(), shown, "old decoder output cannot paint after recovery");
  s.instances[1].output();
  assert.equal(s.drawn(), shown + 1);
  retired.callbacks.error(new Error("late retired decoder error"));
  assert.equal(s.instances[1].state, "configured");
  s.decoder.close();
  s.instances[1].output();
  assert.equal(s.drawn(), shown + 1);
  assert.equal(s.timers.pending.size, 0);
});

test("a starting decoder is not behind until it has caught up, or 2 s have passed", () => {
  const s = videoFixture();
  s.decoder.push(s.packet(true));
  for (let i = 0; i < 15; i++) {
    s.instances[0].decodeQueueSize = i + 1;
    s.decoder.push(s.packet());
  }
  // Its first frame comes out with the start-up queue still waiting: not caught up yet.
  s.instances[0].decodeQueueSize = 15;
  s.instances[0].output();
  s.decoder.push(s.packet());
  assert.equal(s.instances[0].state, "configured");
  assert.equal(s.instances[0].decoded.length, 17);
  assert.equal(s.requested(), 0);
  // Never catching up is judged after 2 s (and, as any lag, dropped once it lasts).
  s.advance(2001);
  s.decoder.push(s.packet());
  s.advance(300);
  s.decoder.push(s.packet());
  assert.equal(s.instances[0].state, "closed");
  assert.equal(s.requested(), 1);
  s.decoder.close();
});

test("a lag under the allowance, or one that clears within 300 ms, is ridden out", () => {
  const s = videoFixture();
  s.decoder.push(s.packet(true));
  s.instances[0].output();
  fill(s);
  s.instances[0].decodeQueueSize = BEHIND - 1;
  s.decoder.push(s.packet());
  s.instances[0].decodeQueueSize = BEHIND;
  s.decoder.push(s.packet());
  s.advance(250);
  s.instances[0].decodeQueueSize = 1;
  s.decoder.push(s.packet());
  // A new stall restarts the clock.
  s.instances[0].decodeQueueSize = BEHIND;
  s.decoder.push(s.packet());
  s.advance(250);
  s.decoder.push(s.packet());
  assert.equal(s.instances[0].state, "configured");
  assert.equal(s.instances[0].decoded.length, BEHIND + 6);
  assert.equal(s.requested(), 0);
  s.decoder.close();
});

test("a lag of a second drops at once", () => {
  const s = videoFixture();
  s.decoder.push(s.packet(true));
  s.instances[0].output();
  fill(s, 70);
  s.instances[0].decodeQueueSize = 61;
  s.decoder.push(s.packet());
  assert.equal(s.instances[0].state, "closed");
  assert.equal(s.requested(), 1);
  s.decoder.close();
});

test("back-to-back keyframes cannot bypass the decoder queue bound", () => {
  const s = videoFixture();
  s.decoder.push(s.packet(true));
  for (let i = 0; i < 5; i++) {
    const previous = s.instances.at(-1);
    previous.output();
    fill(s);
    previous.decodeQueueSize = BEHIND;
    s.decoder.push(s.packet(true));
    assert.equal(previous.state, "closed");
    assert.equal(s.instances.at(-1).decoded.length, 1);
  }
  assert.equal(s.requested(), 0, "the arriving IDR is already sufficient for recovery");
  s.decoder.close();
});

test("HEVC and AV1 keyframes configure their decoder with the coded size; unknown codec ids are dropped", () => {
  const s = videoFixture();
  const packet = (flags, width, height, payload) => {
    const buffer = new ArrayBuffer(18 + payload.length);
    const head = new DataView(buffer);
    head.setUint8(0, 1); head.setUint8(1, flags); head.setUint16(2, width); head.setUint16(4, height);
    head.setUint32(14, payload.length);
    new Uint8Array(buffer, 18).set(payload);
    return buffer;
  };
  // HEVC (codec id 1) keyframe: VPS, SPS, PPS, IDR.
  const hevc = new Uint8Array([0, 0, 0, 1, 0x40, 1, 0, 0, 0, 1, 0x42, 1, 0, 0, 0, 1, 0x44, 1, 0, 0, 0, 1, 0x26, 1]);
  s.decoder.push(packet(0b011, 5120, 2160, hevc));
  assert.equal(s.instances.length, 1);
  assert.match(s.instances[0].config.codec, /^hev1\./);
  assert.equal(s.instances[0].config.codedWidth, 5120);
  assert.equal(s.instances[0].config.codedHeight, 2160);
  assert.equal(s.instances[0].decoded.length, 1);
  // A delta of the same stream reuses the decoder.
  s.decoder.push(packet(0b010, 5120, 2160, new Uint8Array([0, 0, 0, 1, 0x02, 1])));
  assert.equal(s.instances.length, 1);
  assert.equal(s.instances[0].decoded.length, 2);
  // The host falls back to H.264 at a keyframe: a new decoder for the new codec.
  s.decoder.push(packet(0b001, 3840, 1620, new Uint8Array([0, 0, 0, 1, 0x67, 0x64, 0, 0x33, 0, 0, 0, 1, 0x65, 1])));
  assert.equal(s.instances.length, 2);
  assert.equal(s.instances[0].state, "closed");
  assert.equal(s.instances[1].config.codec, "avc1.640033");
  // AV1 (codec id 2): sequence header OBU first.
  s.decoder.push(packet(0b101, 5120, 2160, new Uint8Array([0x0a, 1, 0])));
  assert.match(s.instances[2].config.codec, /^av01\./);
  // A codec id this viewer does not know is not a frame it can use.
  assert.equal(s.parseFrame(packet(0b111, 64, 64, new Uint8Array([1]))), null);
  s.decoder.close();
});
