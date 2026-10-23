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

// A host without the live input channel: the control layer falls back to one call per action.
const olderHostApi = {
  openInput: async () => { throw new Error("older host"); },
  clipboardText: async () => null, clipboardFiles: async () => [], setClipboardText: async () => {},
};
const remoteInputGlobals = { JSON, Set, document: { hasFocus: () => false }, setInterval: () => 0, clearInterval() {} };
function remoteInputDependencies(timers) {
  return {
    "../lib/i18n": { t: (key) => key },
    "../lib/manual-input": module("../apps/desktop/src/lib/manual-input.ts", {}, timers),
    "../lib/live-input": module("../apps/desktop/src/lib/live-input.ts", {}, { JSON, Set }),
    "../lib/platform": { detectOs: () => "mac" },
    "react/jsx-runtime": { jsx: (_tag, props) => ({ props }) },
  };
}

test("buffered input is discarded on leaving takeover; live buffered text still sends", async () => {
  const timers = clock();
  const calls = [];
  const cleanups = [];
  const { RemoteInput } = module("../apps/desktop/src/ui/RemoteInput.tsx", {
    ...remoteInputDependencies(timers),
    react: { useRef: (current) => ({ current }), useEffect: (effect) => cleanups.push(effect()), useState: (v) => [v, () => {}] },
    "../lib/api": { api: { callTool: async (...args) => { calls.push(args); }, ...olderHostApi } },
  }, { ...timers, ...remoteInputGlobals });
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
    ...remoteInputDependencies(timers),
    react: { useRef: (current) => ({ current: refs++ === 0 ? element : current }), useEffect: (effect) => effect(), useState: (v) => [v, () => {}] },
    "../lib/api": { api: { callTool: async (...args) => { calls.push(args); }, ...olderHostApi } },
  }, { ...timers, ...remoteInputGlobals });
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

function videoFixture(options = {}) {
  const timers = clock();
  const instances = [];
  const stats = [];
  const logs = [];
  let now = 0, requested = 0, drawn = 0;
  class MockDecoder {
    constructor(callbacks) { this.callbacks = callbacks; this.state = "unconfigured"; this.decodeQueueSize = 0; this.decoded = []; instances.push(this); }
    configure(config) { this.config = config; options.configure?.(this, config); this.state = "configured"; }
    decode(chunk) { options.decode?.(this, chunk); this.decoded.push(chunk); }
    close() { this.state = "closed"; }
    output(width = 1920, height = 1080) {
      let closed = false;
      this.callbacks.output({ displayWidth: width, displayHeight: height, close: () => { closed = true; } });
      assert.ok(closed, "every decoded frame must be released");
    }
  }
  const codec = module("../apps/desktop/src/lib/codec.ts", {}, timers);
  const exports = module("../apps/desktop/src/lib/video.ts", { "../bindings": { commands: {} }, "./codec": codec, "./hops": { hop() {}, drawTime() {} } }, timers, "", {
    performance: { now: () => now }, VideoDecoder: MockDecoder, EncodedVideoChunk: class { constructor(data) { Object.assign(this, data); } },
    console: { info: (text) => logs.push(text), error: (text) => logs.push(text) },
  });
  const canvas = { width: 0, height: 0, getContext: () => ({ drawImage() { drawn++; } }) };
  const decoder = exports.createDecoder(canvas, () => { requested++; }, () => {}, (value) => stats.push(value));
  // Frames 1/60 s apart in stream time, as a 60 fps host sends them.
  let ts = 0n;
  const packet = (key = false, { width = 1920, height = 1080, profile = 0x42 } = {}) => {
    const payload = new Uint8Array([0, 0, 0, 1, key ? 0x67 : 0x61, profile, 0, 0x28]);
    const buffer = new ArrayBuffer(18 + payload.length);
    const head = new DataView(buffer);
    head.setUint8(0, 1); head.setUint8(1, key ? 1 : 0); head.setUint16(2, width); head.setUint16(4, height); head.setUint32(14, payload.length);
    head.setBigUint64(6, ts); ts += 16_667n;
    new Uint8Array(buffer, 18).set(payload);
    return buffer;
  };
  return { ...exports, decoder, instances, stats, logs, canvas, timers, packet, advance: (n) => { now += n; }, requested: () => requested, drawn: () => drawn };
}

const videoError = (name) => Object.assign(new Error("controlled decoder failure"), { name });
const fallbackLogs = (s) => s.logs.filter((text) => text === "Live video: retrying decoder with default acceleration");

test("decoder starts with hardware preference and retries synchronous configuration failures on the next keyframe", () => {
  for (const name of ["OperationError", "NotSupportedError"]) {
    let first = true;
    const s = videoFixture({ configure() { if (first) { first = false; throw videoError(name); } } });
    s.decoder.push(s.packet(true));
    assert.equal(s.instances[0].config.hardwareAcceleration, "prefer-hardware");
    assert.equal(s.instances[0].state, "closed");
    assert.equal(s.requested(), 1);
    assert.equal(fallbackLogs(s).length, 1);
    s.decoder.push(s.packet());
    assert.equal(s.instances.length, 1, "a delta cannot configure the fallback decoder");
    s.decoder.push(s.packet(true));
    const fallback = s.instances[1];
    assert.equal(fallback.config.hardwareAcceleration, "no-preference");
    assert.equal(fallback.decoded.length, 1);
    fallback.output();
    assert.equal(s.canvas.width, 1920);
    assert.equal(s.canvas.height, 1080);
    assert.equal(s.drawn(), 1, "the fallback presents decoded output normally");
    s.decoder.close();
    fallback.output(640, 480);
    assert.equal(s.drawn(), 1);
    assert.equal(s.canvas.width, 1920);
    assert.equal(fallback.state, "closed");
    assert.equal(s.timers.pending.size, 0);
  }
});

test("asynchronous hardware failures keep the same tuple on default acceleration across resets", () => {
  for (const name of ["OperationError", "NotSupportedError"]) {
    const s = videoFixture();
    s.decoder.push(s.packet(true));
    const hardware = s.instances[0];
    assert.equal(hardware.config.hardwareAcceleration, "prefer-hardware");
    hardware.callbacks.error(videoError(name));
    assert.equal(hardware.state, "closed");
    assert.equal(s.requested(), 1);
    s.decoder.push(s.packet(true));
    assert.equal(s.instances[1].config.hardwareAcceleration, "no-preference");
    // A browser-selected path can still fail: preserve reset/IDR recovery, without returning to HW.
    s.instances[1].callbacks.error(videoError(name));
    s.decoder.push(s.packet(true));
    assert.equal(s.instances[2].config.hardwareAcceleration, "no-preference");
    assert.equal(fallbackLogs(s).length, 1, "one transition is logged for a remembered tuple");
    s.instances[2].output();
    assert.equal(s.drawn(), 1);
    s.decoder.close();
    const count = s.instances.length;
    s.instances[2].callbacks.error(videoError(name));
    s.decoder.push(s.packet(true));
    assert.equal(s.instances.length, count);
    assert.equal(fallbackLogs(s).length, 1);
    assert.equal(s.timers.pending.size, 0);
  }
});

test("only hardware capability failures change acceleration; packet and state errors retain hardware preference", () => {
  for (const name of ["DataError", "EncodingError", "InvalidStateError", "TypeError"]) {
    for (const path of ["configure", "decode", "async"]) {
      let first = true;
      const s = videoFixture(path === "async" ? {} : {
        [path]() { if (first) { first = false; throw videoError(name); } },
      });
      s.decoder.push(s.packet(true));
      if (path === "async") s.instances[0].callbacks.error(videoError(name));
      assert.equal(s.instances[0].state, "closed");
      s.decoder.push(s.packet(true));
      assert.equal(s.instances[1].config.hardwareAcceleration, "prefer-hardware", `${path} ${name}`);
      assert.equal(fallbackLogs(s).length, 0);
      s.decoder.close();
    }
  }
});

test("a synchronous hardware decode failure selects default acceleration for the next keyframe", () => {
  let first = true;
  const s = videoFixture({ decode() { if (first) { first = false; throw videoError("OperationError"); } } });
  s.decoder.push(s.packet(true));
  assert.equal(s.instances[0].state, "closed");
  s.decoder.push(s.packet(true));
  assert.equal(s.instances[1].config.hardwareAcceleration, "no-preference");
  s.instances[1].output();
  assert.equal(s.drawn(), 1);
  assert.equal(fallbackLogs(s).length, 1);
  s.decoder.close();
});

test("retired decoder errors cannot affect a live decoder or poison a future configuration", () => {
  const s = videoFixture();
  s.decoder.push(s.packet(true));
  const retired = s.instances[0];
  s.decoder.push(s.packet(true, { width: 1280, height: 720 }));
  const current = s.instances[1];
  retired.callbacks.error(videoError("OperationError"));
  retired.callbacks.error(videoError("NotSupportedError"));
  assert.equal(current.state, "configured");
  assert.equal(s.requested(), 0);
  assert.equal(fallbackLogs(s).length, 0);
  s.decoder.push(s.packet(true));
  assert.equal(s.instances[2].config.hardwareAcceleration, "prefer-hardware");
  s.decoder.close();
});

test("hardware fallback is scoped to the codec and both coded dimensions", () => {
  const s = videoFixture();
  s.decoder.push(s.packet(true));
  s.instances[0].callbacks.error(videoError("OperationError"));
  for (const changed of [{ width: 1280 }, { height: 720 }, { profile: 0x64 }]) {
    s.decoder.push(s.packet(true, changed));
    assert.equal(s.instances.at(-1).config.hardwareAcceleration, "prefer-hardware");
  }
  s.decoder.push(s.packet(true));
  assert.equal(s.instances.at(-1).config.hardwareAcceleration, "no-preference");
  assert.equal(fallbackLogs(s).length, 1);
  s.decoder.close();
});

test("decoder fallback history retains the most recent 16 failed configurations", () => {
  const s = videoFixture();
  for (let i = 0; i < 17; i++) {
    s.decoder.push(s.packet(true, { width: 1920 + i }));
    assert.equal(s.instances.at(-1).config.hardwareAcceleration, "prefer-hardware");
    s.instances.at(-1).callbacks.error(videoError("OperationError"));
  }
  s.decoder.push(s.packet(true, { width: 1921 }));
  assert.equal(s.instances.at(-1).config.hardwareAcceleration, "no-preference", "the retained oldest tuple stays on fallback");
  s.instances.at(-1).callbacks.error(videoError("OperationError"));
  s.decoder.push(s.packet(true, { width: 1936 }));
  assert.equal(s.instances.at(-1).config.hardwareAcceleration, "no-preference", "repeated fallback failures do not evict history");
  s.decoder.push(s.packet(true));
  assert.equal(s.instances.at(-1).config.hardwareAcceleration, "prefer-hardware", "a tuple outside the bounded recent history can probe hardware again");
  assert.equal(fallbackLogs(s).length, 17);
  s.decoder.close();
});

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

test("the host clock takes the shortest round trip and bounds its error by half of it", () => {
  let now = 1000;
  const { createHostClock } = module("../apps/desktop/src/lib/clock.ts", {}, clock());
  const c = createHostClock(() => now);
  assert.equal(c.toLocalMs(0), null);
  // The host's stream clock reads 500 ms when our clock reads 1010 ms (true offset 510 ms).
  const a = c.ping(); now += 30; c.pong(a, (now - 15 - 510) * 1000 + 5000); // a slow, asymmetric reply
  const b = c.ping(); now += 4; c.pong(b, (now - 2 - 510) * 1000);
  assert.equal(c.errorMs(), 2);
  assert.ok(Math.abs(c.toLocalMs(500_000) - 1010) < 0.001);
  // Unknown and repeated pongs change nothing.
  c.pong(99, 0); c.pong(b, 0);
  assert.ok(Math.abs(c.toLocalMs(500_000) - 1010) < 0.001);
  // A host that never answers does not grow the clock without bound.
  for (let i = 0; i < 1000; i++) c.ping();
});

test("drawn frames report their latency from capture through the host clock", async () => {
  const s = videoFixture();
  s.decoder.close();
  const hostClock = { toLocalMs: (us) => us / 1000 + 100, errorMs: () => 0.5 };
  const decoder = s.createDecoder({ width: 0, height: 0, getContext: () => ({ drawImage() {} }) }, () => {}, () => {},
    (value) => s.stats.push(value), hostClock);
  decoder.push(s.packet(true));
  const instance = s.instances.at(-1);
  // Captured at stream time 0 (local 100 ms), drawn at local 112 ms.
  s.advance(112);
  instance.callbacks.output({ displayWidth: 1920, displayHeight: 1080, timestamp: 0, close() {} });
  s.advance(1000);
  await s.timers.run(1000);
  assert.equal(s.stats.at(-1).latencyMs, 12);
  assert.equal(s.stats.at(-1).latencyErrorMs, 0.5);
  decoder.close();
});

test("adaptive bitrate cuts to what arrives, holds while the queue drains, and climbs back when calm", () => {
  const { createRateController } = module("../apps/desktop/src/lib/rate.ts", {}, clock());
  const applied = [];
  const rate = createRateController(12_000, (kbps, why) => applied.push({ kbps, why }));
  let now = 0;
  // 60 fps for `seconds` at `latency(t)` ms, with `kbps` arriving.
  const run = (seconds, latency, kbps = 9_000) => {
    for (let i = 0; i < seconds * 60; i++) {
      now += 1000 / 60;
      rate.received(kbps * 1000 / 8 / 60, now);
      rate.observe(latency(now), now);
    }
  };
  run(5, () => 15);
  assert.deepEqual(applied, [], "a steady latency changes nothing");
  // The link carries 8 Mbit/s: the queue grows 100 ms a second.
  const congested = now;
  run(2, (t) => 15 + (t - congested) / 10, 8_000);
  assert.equal(applied.length, 1, "one cut once the queue lasted a second");
  assert.equal(applied[0].kbps, 6_800, "to what arrives, with room to drain");
  assert.match(applied[0].why, /8000 kbit\/s arriving/);
  // Draining: a long queue that shrinks is left alone.
  const peak = 15 + (now - congested) / 10;
  const draining = now;
  run(2, (t) => Math.max(15, peak - (t - draining) / 5), 8_000);
  assert.equal(applied.length, 1, "no cut while the queue drains");
  // Calm: back up in steps, but under 90% of what queued for 20 s after the cut, then to the request.
  run(15, () => 15);
  assert.ok(applied.length > 1 && applied.every((a, i) => i === 0 || a.kbps > applied[i - 1].kbps));
  assert.ok(rate.current() <= 10_800, `held under what queued: ${rate.current()}`);
  run(20, () => 15);
  assert.equal(rate.current(), 12_000);
});

test("adaptive bitrate never goes under its floor", () => {
  const { createRateController } = module("../apps/desktop/src/lib/rate.ts", {}, clock());
  const rate = createRateController(40_000, () => {});
  let now = 0;
  for (let i = 0; i < 30 * 60; i++) {
    now += 1000 / 60;
    rate.received(100, now);
    rate.observe(15 + now / 5, now);
  }
  assert.equal(rate.current(), 4_000, "a tenth of the request");
});

test("adaptive bitrate ignores a busy machine: late frames that are not a queue", () => {
  const { createRateController } = module("../apps/desktop/src/lib/rate.ts", {}, clock());
  const applied = [];
  const rate = createRateController(12_000, (kbps) => applied.push(kbps));
  let now = 0;
  let held = [];
  // Every 4 s nothing is delivered for 800 ms; then the held frames arrive at once.
  for (let i = 0; i < 40 * 60; i++) {
    now += 1000 / 60;
    held.push(now);
    if (now % 4000 < 800) continue;
    for (const captured of held) {
      rate.received(9_000 * 1000 / 8 / 60, now);
      rate.observe(now - captured + 12, now);
    }
    held = [];
  }
  assert.deepEqual(applied, []);
});

test("adaptive bitrate ignores a single slow frame", () => {
  const { createRateController } = module("../apps/desktop/src/lib/rate.ts", {}, clock());
  const applied = [];
  const rate = createRateController(20_000, (kbps) => applied.push(kbps));
  let now = 0;
  for (let i = 0; i < 600; i++) {
    now += 1000 / 60;
    rate.observe(i % 120 === 0 ? 400 : 12, now);
  }
  assert.deepEqual(applied, []);
});

/** The live view over a link of `linkKbps` (a queue in front of it) for `seconds` at 60 fps:
 * bitrate requested over time and latency, as the controller saw them. */
function simulateLink(createRateController, requestedKbps, linkKbps, seconds, baseMs = 15) {
  let target = requestedKbps;
  const rate = createRateController(requestedKbps, (kbps) => { target = kbps; });
  let queued = 0; // kbit
  const samples = [];
  for (let i = 1; i <= seconds * 60; i++) {
    const now = i * 1000 / 60;
    queued += target / 60;
    const sent = Math.min(queued, linkKbps(now) / 60);
    queued -= sent;
    rate.received(sent * 1000 / 8, now);
    const latency = baseMs + queued / linkKbps(now) * 1000;
    rate.observe(latency, now);
    samples.push({ now, target, latency });
  }
  return samples;
}

test("adaptive bitrate settles under a slower link without collapsing to the floor", () => {
  const { createRateController } = module("../apps/desktop/src/lib/rate.ts", {}, clock());
  const samples = simulateLink(createRateController, 12_000, () => 8_000, 60);
  const lowest = Math.min(...samples.map((s) => s.target));
  assert.ok(lowest >= 3_000, `never far under the link: lowest ${lowest} kbit/s`);
  const late = samples.filter((s) => s.now > 30_000);
  const meanTarget = late.reduce((sum, s) => sum + s.target, 0) / late.length;
  const sortedLatency = late.map((s) => s.latency).sort((a, b) => a - b);
  assert.ok(meanTarget > 5_500 && meanTarget <= 8_400, `uses most of the link: ${Math.round(meanTarget)} kbit/s`);
  assert.ok(sortedLatency[Math.floor(sortedLatency.length / 2)] < 15 + 60, "the queue stays drained");
});

test("adaptive bitrate follows a link that drops and recovers", () => {
  const { createRateController } = module("../apps/desktop/src/lib/rate.ts", {}, clock());
  const link = (now) => (now > 10_000 && now < 30_000 ? 4_000 : 20_000);
  const samples = simulateLink(createRateController, 16_000, link, 70);
  const at = (t) => samples.find((s) => s.now >= t);
  assert.equal(at(9_000).target, 16_000, "untouched while the link has room");
  assert.ok(at(29_000).target <= 4_400, `about the narrow link: ${at(29_000).target}`);
  assert.ok(at(29_000).latency < 15 + 100, `drained under the narrow link: ${at(29_000).latency}`);
  assert.equal(at(70_000).target, 16_000, "back to the request once the link recovers");
});

test("adaptive bitrate sees a queue that formed before the first latency, through the ping delay", () => {
  const { createRateController } = module("../apps/desktop/src/lib/rate.ts", {}, clock());
  const applied = [];
  const rate = createRateController(12_000, (kbps) => applied.push(kbps));
  rate.pathDelay(1);
  let now = 0;
  // The first latency known is already 800 ms of queue, growing; 8 Mbit/s arrives.
  for (let i = 0; i < 90; i++) {
    now += 1000 / 60;
    rate.received(8_000 * 1000 / 8 / 60, now);
    rate.observe(800 + now / 2, now);
  }
  assert.equal(applied[0], 4_800, "a deep cut within 1.5 s: 0.6 of what arrives");
});
