// Stream transport and React lifecycle regressions, with the real desktop modules transpiled.
// WebCodecs and Tauri are controlled fixtures; these checks do not claim live-host performance.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

const tick = () => new Promise((resolve) => setImmediate(resolve));
const plain = (value) => JSON.parse(JSON.stringify(value));

function module(path, dependencies = {}, globals = {}) {
  const source = readFileSync(new URL(path, import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022,
  } }).outputText;
  const exports = {};
  vm.runInNewContext(compiled, {
    exports,
    require(name) {
      assert.ok(name in dependencies, `unexpected dependency ${name}`);
      return dependencies[name];
    },
    ArrayBuffer, Uint8Array, TextDecoder, Error, console,
    ...globals,
  }, { filename: path });
  return exports;
}

function apiFixture(reply = { screen: { x: 0, y: 0, width: 1600, height: 1000 } }) {
  const channels = [], requests = [], frames = [], ended = [];
  class Channel { constructor() { channels.push(this); } }
  const commands = {
    async watchScreen(host, request, channel) {
      requests.push({ host, request, channel });
      return { status: "ok", data: [42, reply] };
    },
    async requestKeyframe() {}, async stopScreen() {},
  };
  const { api } = module("../apps/desktop/src/lib/api.ts", {
    "@tauri-apps/api/core": { Channel }, "../bindings": { commands }, "./demo": { demoBackend: { kind: "demo" } },
  }, { window: { __TAURI_INTERNALS__: {} } });
  return {
    channels, requests, frames, ended, api,
    start: () => api.watchScreen("test-host", (frame) => frames.push(frame), {
      maxFps: 60, maxLongEdge: 1920, bitrateKbps: 10000, onEnded: (reason) => ended.push(reason),
    }),
  };
}

test("native ArrayBuffer and Vec terminal markers decode UTF-8 without reaching the video decoder", async () => {
  const s = apiFixture();
  await s.start();
  const video = Uint8Array.from([1, 9, 8]);
  s.channels[0].onmessage(video.buffer);
  s.channels[0].onmessage(Array.from(video));
  const reason = "视频连接已结束";
  const terminal = Uint8Array.from([2, ...new TextEncoder().encode(reason)]);
  s.channels[0].onmessage(terminal.buffer);
  s.channels[0].onmessage(Array.from(terminal));
  assert.equal(s.frames.length, 2);
  assert.deepEqual(s.frames.map((frame) => [...new Uint8Array(frame)]), [[1, 9, 8], [1, 9, 8]]);
  assert.deepEqual(s.ended, [reason, reason]);
});

test("malformed IPC byte arrays and non-binary messages do not cause a terminal event", async () => {
  const s = apiFixture();
  await s.start();
  for (const message of [[2, 300], [2, -1], [2, 1.5], [2, NaN], [2, "x"], null, "ended", {}]) {
    s.channels[0].onmessage(message);
  }
  assert.equal(s.frames.length, 0);
  assert.equal(s.ended.length, 0);
});

test("native request serializes only profile fields, excluding the onEnded callback", async () => {
  const s = apiFixture();
  await s.start();
  assert.equal(s.requests[0].host, "test-host");
  assert.deepEqual(plain(s.requests[0].request), { monitor: null, max_fps: 60, max_long_edge: 1920, bitrate_kbps: 10000 });
  assert.ok(!("onEnded" in s.requests[0].request));
});

test("the pointer is asked for, and its positions and shapes are parsed apart from video", async () => {
  const s = apiFixture({ screen: { x: 0, y: 0, width: 1600, height: 1000 }, cursor: "client" });
  const positions = [], shapes = [];
  const view = await s.api.watchScreen("test-host", (frame) => s.frames.push(frame), {
    onPointer: (at) => positions.push(at), onPointerShape: (shape) => shapes.push(shape),
  });
  assert.equal(s.requests[0].request.cursor, "client");
  assert.equal(view.pointer, true);
  const header = (kind, w, h, body) => Uint8Array.from([kind, 0, 0, w, 0, h, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, body.length, ...body]);
  // A 2x1 shape: id 7, hot spot (1, 0), BGRA pixels become RGBA.
  s.channels[0].onmessage(header(5, 2, 1, [0, 0, 0, 7, 0, 1, 0, 0, 10, 20, 30, 255, 40, 50, 60, 0]).buffer);
  s.channels[0].onmessage(header(4, 0, 0, [...new TextEncoder().encode('{"x":5,"y":6,"visible":true,"shape":7}')]).buffer);
  // A shape whose pixels do not match its size is dropped.
  s.channels[0].onmessage(header(5, 3, 3, [0, 0, 0, 8, 0, 0, 0, 0, 1, 2, 3, 4]).buffer);
  assert.equal(s.frames.length, 0, "pointer messages never reach the video decoder");
  assert.deepEqual(plain(positions), [{ x: 5, y: 6, visible: true, shape: 7 }]);
  assert.equal(shapes.length, 1);
  assert.deepEqual([shapes[0].id, shapes[0].width, shapes[0].height, shapes[0].hotX, shapes[0].hotY], [7, 2, 1, 1, 0]);
  assert.deepEqual([...shapes[0].rgba], [30, 20, 10, 255, 60, 50, 40, 0]);
  // An older host does not answer `cursor`: the pointer stays in the picture.
  const old = apiFixture();
  const oldView = await old.api.watchScreen("test-host", () => {}, { onPointer: () => {} });
  assert.equal(oldView.pointer, false);
});

const jsx = (type, props, key) => ({ type, props, key });
const jsxRuntime = { jsx, jsxs: jsx, Fragment: Symbol("Fragment") };

function hooksFixture(canvas) {
  const hooks = [];
  let cursor = 0, refs = 0;
  const react = {
    useRef(value) {
      const index = cursor++;
      if (!(index in hooks)) hooks[index] = { current: canvas && refs++ === 0 ? canvas : value };
      return hooks[index];
    },
    useState(value) {
      const index = cursor++;
      if (!(index in hooks)) hooks[index] = typeof value === "function" ? value() : value;
      return [hooks[index], (next) => { hooks[index] = typeof next === "function" ? next(hooks[index]) : next; }];
    },
    useMemo(compute) { return compute(); },
    useEffect(effect, dependencies) {
      const index = cursor++, previous = hooks[index];
      if (previous && dependencies.every((value, i) => value === previous.dependencies[i])) return;
      previous?.cleanup?.();
      hooks[index] = { dependencies, cleanup: effect() };
    },
  };
  return {
    react,
    render: (component, props) => { cursor = 0; return component(props); },
    close: () => { for (const hook of hooks) hook?.cleanup?.(); },
  };
}

function sessionModule(hooks, api, createDecoder) {
  const storage = new Map();
  const quality = module("../apps/desktop/src/lib/stream-quality.ts", {}, {
    localStorage: { getItem: (key) => storage.get(key) ?? null, setItem: (key, value) => storage.set(key, value) },
  });
  const RemoteInput = () => {};
  const exported = module("../apps/desktop/src/ui/Session.tsx", {
    react: hooks.react,
    "react/jsx-runtime": jsxRuntime,
    "../lib/api": { api },
    "../bindings": { commands: { logFromPage: async () => {} } },
    "../lib/demo": { demoScreenUrl: () => "preview-image" },
    "../lib/i18n": { t: (key) => key, currentLang: () => "en" },
    "../lib/stream-quality": quality,
    "../lib/video": { createDecoder },
    "../lib/codec": { preferredCodecs: () => ["hevc", "h264"] },
    "../lib/clock": { createHostClock: () => ({ pong() {}, ping: () => 0, toLocalMs: () => null, errorMs: () => null }),
      startPinging: () => () => {} },
    "../lib/pointer": { createPointerLayer: () => ({ shape() {}, move() {}, hide() {} }) },
    "../lib/rate": { createRateController: () => ({ observe() {}, received() {}, pathDelay() {}, current: () => Infinity }) },
    "./Decide": { DecisionCard: () => {} },
    "./RemoteInput": { RemoteInput },
    "./glyphs": { Glyph: () => {}, Pointer: () => {}, SeatMark: () => {} },
    "./Icon": { Icon: () => {} },
    "./Shell": { computerFacts: () => [], Facts: () => {}, NavRail: () => {}, TitleBar: () => {} },
    "./TurnLine": { TurnLine: () => {}, turnSpec: () => null },
    "./util": { agentLabel: () => "Test agent", clock: () => "12:00", countdown: () => "1:00", cx: (...values) => values.filter(Boolean).join(" "),
      relayNote: () => null,
      useNow: () => 1000, useSize: () => [{ current: null }, { w: 1000, h: 700 }] },
  }, { localStorage: { getItem: () => null }, performance });
  return { ...exported, RemoteInput };
}

function nodes(tree) {
  if (!tree || typeof tree !== "object") return [];
  const children = tree.props?.children;
  return [tree, ...(Array.isArray(children) ? children : [children]).flatMap(nodes)];
}

function streamFixture(kind = "tauri") {
  const canvas = { width: 1600, height: 1000 };
  const hooks = hooksFixture(canvas);
  const views = [], decoders = [], errors = [], sizes = [], stats = [], screens = [];
  let resets = 0, hasFrame = false;
  const api = {
    kind,
    watchScreen(host, onFrame, options) {
      let resolve, reject;
      const promise = new Promise((done, failed) => { resolve = done; reject = failed; });
      const view = { stops: 0, requestKeyframe() {}, ping() {}, setBitrate() {}, stop() { this.stops++; }, screen: { x: 0, y: 0, width: 1600, height: 1000 } };
      views.push({ host, onFrame, options, resolve, reject, view });
      return promise;
    },
  };
  const createDecoder = (_canvas, _onNeedKeyframe, onSize, onStats) => {
    const decoder = { onSize, onStats, pushed: 0, closed: 0, push() { this.pushed++; }, close() { this.closed++; } };
    decoders.push(decoder);
    return decoder;
  };
  const { Stream } = sessionModule(hooks, api, createDecoder);
  let props = { host: "test-host", maxFps: 60, maxLongEdge: 1920, bitrateKbps: 10000,
    onFrame: () => { hasFrame = true; }, onSize: (...value) => sizes.push(value), onScreen: (value) => screens.push(value),
    onStats: (value) => stats.push(value), onRestart: () => { resets++; hasFrame = false; }, onError: (value) => errors.push(value),
  };
  return {
    canvas, views, decoders, errors, sizes, stats, screens,
    render: () => hooks.render(Stream, props),
    update: (next) => { props = { ...props, ...next }; },
    hasFrame: () => hasFrame, resets: () => resets, close: hooks.close,
  };
}

test("zero FPS on a static screen does not invent a disconnected stream", async () => {
  const s = streamFixture();
  s.render();
  s.views[0].resolve(s.views[0].view); await tick();
  s.decoders[0].onSize(1600, 1000);
  s.decoders[0].onStats({ width: 1600, height: 1000, decodedFps: 0, receivedKbps: 0, droppedFrames: 0, decodeQueueSize: 0 });
  assert.equal(s.hasFrame(), true);
  assert.ok(!nodes(s.render()).some((node) => node.props?.className === "stream-ended"));
  assert.equal(s.decoders[0].closed, 0);
  s.close();
});

test("terminal event clears old pixels, geometry, stats and frame state; retry preserves requested profile", async () => {
  const s = streamFixture();
  s.render(); s.views[0].resolve(s.views[0].view); await tick();
  s.decoders[0].onSize(1600, 1000);
  s.canvas.width = 1600; s.canvas.height = 1000;
  s.views[0].options.onEnded("transport interrupted");
  assert.equal(s.hasFrame(), false);
  assert.equal(s.canvas.width, 0); assert.equal(s.canvas.height, 0);
  assert.equal(s.screens.at(-1), null); assert.equal(s.stats.at(-1), null);
  assert.equal(s.errors.at(-1), "transport interrupted");
  assert.ok(s.decoders[0].closed >= 1); assert.ok(s.views[0].view.stops >= 1);
  const overlay = nodes(s.render()).find((node) => node.props?.className === "stream-ended");
  assert.ok(overlay);
  nodes(overlay).find((node) => node.type === "button").props.onClick();
  s.render();
  assert.equal(s.views.length, 2);
  assert.equal(s.views[1].options.maxFps, 60);
  assert.equal(s.views[1].options.maxLongEdge, 1920);
  assert.equal(s.views[1].options.bitrateKbps, 10000);
  assert.ok(!nodes(s.render()).some((node) => node.props?.className === "stream-ended"));
  s.close();
});

test("quality restart invalidates old geometry and pixels, and retired callbacks cannot affect the new stream", async () => {
  const s = streamFixture();
  s.render(); s.views[0].resolve(s.views[0].view); await tick();
  s.decoders[0].onSize(1600, 1000);
  s.canvas.width = 1600; s.canvas.height = 1000;
  s.update({ maxFps: 30, maxLongEdge: 3840, bitrateKbps: 20000 }); s.render();
  assert.equal(s.views.length, 2);
  assert.equal(s.hasFrame(), false);
  assert.equal(s.canvas.width, 0); assert.equal(s.canvas.height, 0);
  assert.equal(s.screens.at(-1), null); assert.equal(s.stats.at(-1), null);
  const errors = s.errors.length, sizes = s.sizes.length, stats = s.stats.length;
  s.views[0].options.onEnded("retired end"); s.views[0].onFrame(new ArrayBuffer(1));
  s.decoders[0].onSize(800, 600); s.decoders[0].onStats({ decodedFps: 100 });
  assert.equal(s.errors.length, errors); assert.equal(s.sizes.length, sizes); assert.equal(s.stats.length, stats);
  assert.equal(s.decoders[0].pushed, 0);
  s.close();
});

test("a changed callback identity does not restart capture, and a late start reply is stopped", async () => {
  const s = streamFixture();
  s.render();
  s.update({ onFrame: () => {}, onError: () => {}, onSize: () => {} }); s.render();
  assert.equal(s.views.length, 1);
  s.close();
  s.views[0].resolve(s.views[0].view); await tick();
  assert.equal(s.views[0].view.stops, 1);
});

test("terminal events before native start resolves stop the late view and surface a reconnect control", async () => {
  const s = streamFixture();
  s.render();
  // End before native watch resolves: the eventual view must also be stopped.
  s.views[0].options.onEnded("capture permission was refused");
  assert.ok(nodes(s.render()).some((node) => node.props?.className === "stream-ended"));
  s.views[0].resolve(s.views[0].view); await tick();
  assert.equal(s.views[0].view.stops, 1);
  assert.match(s.errors.at(-1), /permission/);
  s.close();
});

test("a rejected native watch request surfaces its reason and a reconnect control", async () => {
  const s = streamFixture();
  s.render();
  s.views[0].reject(new Error("screen capture permission was refused")); await tick();
  assert.equal(s.hasFrame(), false);
  assert.match(s.errors.at(-1), /permission/);
  assert.ok(nodes(s.render()).some((node) => node.props?.className === "stream-ended"));
  assert.ok(s.decoders[0].closed >= 1);
  s.close();
});

test("preview renders a keyed still image without a native stream or fake terminal state", () => {
  const s = streamFixture("demo");
  const first = s.render();
  assert.equal(first.type, "img"); assert.equal(s.views.length, 0); assert.equal(s.decoders.length, 0);
  s.update({ maxFps: 30, maxLongEdge: 3840, bitrateKbps: 20000 });
  const second = s.render();
  assert.equal(second.type, "img");
  assert.notEqual(first.key, second.key, "quality changes remount the still image so onLoad refills geometry");
  assert.equal(s.views.length, 0); assert.equal(s.errors.length, 0);
  s.close();
});

test("manual input mounts only for a human driver with a decoded frame and screen geometry", () => {
  const hooks = hooksFixture();
  const { SessionPage, Stream, RemoteInput } = sessionModule(hooks, { kind: "tauri" }, () => assert.fail("nested Stream should not execute in the page fixture"));
  const c = { host: { name: "test-host", path: "direct" }, os: "mac", online: true, feed: {
    agent: null, pending: [], sessions: {}, steps: [], turns: [], task: null,
    driver: { state: "person", device: "test-device", since_ms: 1000, agent_may_watch: false },
  } };
  const props = { c, device: "test-device", waiting: 0, onBack() {}, onNav() {} };
  const render = () => hooks.render(SessionPage, props);
  let tree = render();
  assert.ok(!nodes(tree).some((node) => node.type === RemoteInput));
  const stream = nodes(tree).find((node) => node.type === Stream);
  stream.props.onScreen({ x: 0, y: 0, width: 1600, height: 1000 }); tree = render();
  assert.ok(!nodes(tree).some((node) => node.type === RemoteInput), "geometry alone is insufficient");
  stream.props.onScreen(null);
  stream.props.onFrame(); tree = render();
  assert.ok(!nodes(tree).some((node) => node.type === RemoteInput), "a frame alone is insufficient");
  stream.props.onScreen({ x: 0, y: 0, width: 1600, height: 1000 }); tree = render();
  assert.ok(nodes(tree).some((node) => node.type === RemoteInput));
  nodes(tree).find((node) => node.type === Stream).props.onRestart(); tree = render();
  assert.ok(!nodes(tree).some((node) => node.type === RemoteInput), "restart removes blind input");
  const restarted = nodes(tree).find((node) => node.type === Stream);
  restarted.props.onFrame(); restarted.props.onScreen({ x: 0, y: 0, width: 1600, height: 1000 });
  c.feed.driver = { state: "agent" }; tree = render();
  assert.ok(!nodes(tree).some((node) => node.type === RemoteInput), "an agent driver never mounts human input");
  hooks.close();
});
