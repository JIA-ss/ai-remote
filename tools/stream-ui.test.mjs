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
    "@tauri-apps/api/core": { Channel }, "../bindings": { commands }, "./demo": { demoBackend: { kind: "demo" } }, "./hops": { hop() {} }, "./chat": { chatLineKind: () => null, requester: () => ({ ask: async () => ({}), reply: () => false, close() {} }) },
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

function hooksFixture(canvas, pointerCanvas) {
  const hooks = [];
  let cursor = 0, refs = 0;
  const react = {
    useRef(value) {
      const index = cursor++;
      if (!(index in hooks)) {
        const n = refs++;
        hooks[index] = { current: canvas && n === 0 ? canvas : pointerCanvas && n === 1 ? pointerCanvas : value };
      }
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
      if (previous && dependencies && previous.dependencies && dependencies.every((value, i) => value === previous.dependencies[i])) return;
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

function sessionModule(hooks, api, createDecoder, storage = new Map()) {
  const localStorage = { getItem: (key) => storage.get(key) ?? null, setItem: (key, value) => storage.set(key, value) };
  const quality = module("../apps/desktop/src/lib/stream-quality.ts", {}, { localStorage });
  const monitors = module("../apps/desktop/src/lib/monitors.ts", {}, { localStorage });
  const RemoteInput = () => {};
  const TitleBar = () => {};
  const AgentChat = () => null;
  const DecisionCard = () => {};
  const exported = module("../apps/desktop/src/ui/Session.tsx", {
    react: hooks.react,
    "react/jsx-runtime": jsxRuntime,
    "../lib/api": { api },
    "../bindings": { commands: { logFromPage: async () => {} } },
    "../lib/demo": { demoScreenUrl: () => "preview-image", demoMonitors: [{ index: 0, x: 0, y: 0, width: 1600, height: 1000, primary: true }] },
    "../lib/i18n": { t: (key) => key, currentLang: () => "en" },
    "../lib/stream-quality": quality,
    "../lib/monitors": monitors,
    "../lib/video": { createDecoder },
    "../lib/codec": { preferredCodecs: () => ["hevc", "h264"] },
    "../lib/clock": { createHostClock: () => ({ pong() {}, ping: () => 0, toLocalMs: () => null, errorMs: () => null }),
      startPinging: () => () => {} },
    "../lib/pointer": { createPointerLayer: () => ({ shape() {}, move: (at) => at.visible, hide() {} }) },
    "../lib/rate": { createRateController: () => ({ observe() {}, received() {}, pathDelay() {}, current: () => Infinity }) },
    "./AgentChat": { AgentChat },
    "./Decide": { DecisionCard },
    "./RemoteInput": { RemoteInput },
    "../lib/drop": { watchFileDrop: () => () => {} },
    "./glyphs": { Glyph: () => {}, Pointer: () => {}, SeatMark: () => {} },
    "./Icon": { Icon: () => {} },
    "./Shell": { computerFacts: () => [], Facts: () => {}, TitleBar },
    "../lib/fullscreen": { setFullscreen: async () => {}, isFullscreen: async () => false, watchFullscreen: () => () => {} },
    "motion/react": { AnimatePresence: ({ children }) => children, motion: { div: "div" } },
    "./util": { agentLabel: () => "Test agent", clock: () => "12:00", countdown: () => "1:00", cx: (...values) => values.filter(Boolean).join(" "),
      relayNote: () => null,
      useNow: () => 1000, useSize: () => [{ current: null }, { w: 1000, h: 700 }] },
  }, { localStorage: { getItem: () => null }, performance,
    // The page registers its full-screen keys on the window and sizes the picture by device pixels.
    window: { addEventListener() {}, removeEventListener() {}, devicePixelRatio: 1, setTimeout, clearTimeout } });
  return { ...exported, RemoteInput, TitleBar, AgentChat, DecisionCard };
}

function nodes(tree) {
  if (!tree || typeof tree !== "object") return [];
  const children = tree.props?.children;
  return [tree, ...(Array.isArray(children) ? children : [children]).flatMap(nodes)];
}

function streamFixture(kind = "tauri", { pointerCanvas } = {}) {
  const canvas = { width: 1600, height: 1000 };
  const hooks = hooksFixture(canvas, pointerCanvas);
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

test("the local cursor hides only while the host's pointer is drawn, not while the host reports it hidden", async () => {
  const s = streamFixture("tauri", { pointerCanvas: {} });
  const shown = [];
  s.update({ onPointerShown: (on) => shown.push(on) });
  s.render();
  s.views[0].view.pointer = true;
  s.views[0].resolve(s.views[0].view); await tick();
  // Windows suppressing its pointer (touch or pen input) reports it hidden: keep your own cursor.
  s.views[0].options.onPointer({ x: 10, y: 10, visible: false, shape: 0 });
  assert.deepEqual(shown, []);
  s.views[0].options.onPointer({ x: 12, y: 10, visible: true, shape: 1 });
  assert.deepEqual(shown, [true]);
  s.views[0].options.onPointer({ x: 14, y: 10, visible: true, shape: 1 });
  assert.deepEqual(shown, [true], "reported on change only");
  s.views[0].options.onPointer({ x: 14, y: 10, visible: false, shape: 1 });
  assert.deepEqual(shown, [true, false]);
  s.views[0].options.onPointer({ x: 16, y: 10, visible: true, shape: 1 });
  s.views[0].options.onEnded("transport interrupted");
  assert.deepEqual(shown, [true, false, true, false], "an ended stream gives the cursor back");
  s.close();
});

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

test("the live view always shows the existing conversation and keeps approvals reachable after full screen", () => {
  const hooks = hooksFixture();
  const { SessionPage, TitleBar, AgentChat, DecisionCard } = sessionModule(hooks, { kind: "demo" }, () => {});
  const c = { host: { name: "test-host", path: "direct" }, os: "mac", online: true, feed: {
    agent: null, pending: [], sessions: {}, steps: [], turns: [], task: null, driver: { state: "agent" },
  } };
  const chosen = { session: "from-workspace" };
  const props = { c, device: "test-device", waiting: 0, onBack() {}, onNav() {}, chat: chosen };
  const render = () => hooks.render(SessionPage, props);
  let tree = render();
  const rail = nodes(tree).find((node) => node.type === "aside");
  assert.equal(rail.props.className, "nt-rail wide");
  assert.equal(nodes(rail).filter((node) => node.type === AgentChat).length, 1);
  assert.equal(nodes(rail).find((node) => node.type === AgentChat).props.target, chosen);
  assert.ok(nodes(rail).every((node) => node.props?.role !== "tablist" && node.props?.role !== "tab"));
  assert.ok(nodes(rail).every((node) => node.props?.className !== "composer-foot"));
  assert.ok(!nodes(tree).some((node) => node.type === DecisionCard));

  c.feed.pending = [{ id: "approval-1", tool: "shell", expiresMs: 10_000 }];
  c.feed.steps = [{ approvalId: "approval-1", startedMs: 500 }];
  tree = render();
  const approval = nodes(tree).find((node) => node.type === DecisionCard);
  assert.equal(approval.props.p.id, "approval-1");
  assert.equal(approval.props.askedMs, 500);
  assert.equal(nodes(tree).find((node) => node.props?.className === "nt-chat-approval").props.ref, approval.props.escapeScope,
    "the wrapper focused by Review is the card's Escape scope");
  assert.ok(nodes(tree).indexOf(approval) < nodes(tree).findIndex((node) => node.type === AgentChat), "approval precedes the conversation");

  nodes(tree).find((node) => node.type === TitleBar).props.end.props.children.at(-1).props.onClick();
  tree = render();
  assert.ok(!nodes(tree).some((node) => node.type === "aside"), "full screen keeps the picture clear");
  const review = nodes(tree).find((node) => node.type === "button" && node.props.children === "review");
  assert.ok(review, "the stage still offers to review the pending action");
  review.props.onClick();
  tree = render();
  assert.ok(nodes(tree).some((node) => node.type === DecisionCard));
  assert.ok(nodes(tree).some((node) => node.type === AgentChat));
  hooks.close();
});

test("host approval Escape stays with its card beside chat and respects the real find bar", async () => {
  class Element {
    constructor(parent = null) { this.parent = parent; }
    contains(node) {
      for (let at = node; at; at = at.parent) if (at === this) return true;
      return false;
    }
  }
  const window = new EventTarget();
  const document = { activeElement: null };
  const denied = [], mounted = [];
  const approvalRoot = new Element(), approvalButton = new Element(approvalRoot), chatField = new Element();
  const scope = { current: approvalRoot };
  const dependencies = (hooks) => ({
    react: hooks.react, "react/jsx-runtime": jsxRuntime,
    "../lib/api": { api: { approverStatus: async () => true, deny: async (host, id) => { denied.push([host, id]); } } },
    "../lib/i18n": { currentLang: () => "en", t: (key) => key },
    "../lib/platform": { detectOs: () => "mac" },
    "./glyphs": { Actor: () => null, Pointer: () => null }, "./Icon": { Icon: () => null },
    "./util": { clock: () => "12:00", commandOf: () => "echo ok", countdown: () => "1:00",
      cx: (...xs) => xs.filter(Boolean).join(" "), destructive: () => false, useNow: () => 1000 },
  });
  const mount = (id, escapeScope) => {
    const hooks = hooksFixture();
    const { DecisionCard } = module("../apps/desktop/src/ui/Decide.tsx", dependencies(hooks), { window, document, Node: Element });
    hooks.render(DecisionCard, { p: { id, tool: "shell", argsPreview: {}, expiresMs: 10_000 }, host: "test-host", agent: "test-agent", escapeScope });
    mounted.push(hooks);
    return hooks;
  };
  const escape = (target, repeat = false) => {
    const event = new Event("keydown", { cancelable: true });
    Object.defineProperties(event, { key: { value: "Escape" }, target: { value: target }, repeat: { value: repeat } });
    return event;
  };

  // Load the actual shared FindBar, used by both Claude and Codex, and bubble its handled key.
  const findHooks = hooksFixture();
  const { FindBar } = module("../apps/desktop/src/ui/chat/Transcript.tsx", {
    react: { ...findHooks.react, memo: (fn) => fn }, "react/jsx-runtime": jsxRuntime,
    "lucide-react": {}, "../../lib/chat-input": {}, "../../lib/chat": {},
    "../../lib/i18n": { t: (key) => key }, "../util": { cx: () => "" },
    "./Rows": {}, "../codex/CodexTurn": {},
  });
  let closed = 0;
  const find = findHooks.render(FindBar, { root: { current: null }, focus: 0, onShow() {}, onClose: () => closed++ });
  const input = nodes(find).find((node) => node.type === "input");
  const scoped = mount("beside-chat", scope);
  document.activeElement = chatField;
  const findKey = escape(chatField);
  input.props.onKeyDown(findKey);
  window.dispatchEvent(findKey);
  assert.equal(closed, 1, "Escape closes the real chat find bar");
  assert.equal(findKey.defaultPrevented, true);
  assert.deepEqual(denied, [], "closing find cannot reject the adjacent host request");

  window.dispatchEvent(escape(chatField));
  assert.deepEqual(denied, [], "even an unhandled Escape outside the local scope cannot reject it");
  document.activeElement = approvalButton;
  const cardKey = escape(approvalButton);
  window.dispatchEvent(cardKey);
  assert.equal(cardKey.defaultPrevented, true, "the card consumes its own Escape");
  assert.deepEqual(denied, [["test-host", "beside-chat"]]);
  window.dispatchEvent(escape(approvalButton, true));
  assert.equal(denied.length, 1, "holding Escape does not repeat the rejection");
  scoped.close();

  mount("standalone");
  mount("another-standalone");
  document.activeElement = chatField;
  const consumed = escape(chatField);
  consumed.preventDefault();
  window.dispatchEvent(consumed);
  assert.equal(denied.length, 1, "standalone approvals also honor a key another control consumed");
  window.dispatchEvent(escape(chatField));
  assert.deepEqual(denied, [["test-host", "beside-chat"], ["test-host", "standalone"]],
    "legacy standalone cards retain Escape, and only the first consumes a key");
  await tick();
  for (const hooks of mounted) hooks.close();
  findHooks.close();
});

test("takeover options live over the stage and preserve watch and hand-back settings", async () => {
  const hooks = hooksFixture();
  const calls = [];
  const { SessionPage, AgentChat } = sessionModule(hooks, {
    kind: "demo", setTurn: async (...args) => { calls.push(args); },
  }, () => {});
  const c = { host: { name: "test-host", path: "direct" }, os: "mac", online: true, feed: {
    agent: null, pending: [], sessions: {}, steps: [], turns: [], task: null, driver: { state: "person" },
  } };
  const props = { c, device: "test-device", waiting: 0, onBack() {}, onNav() {} };
  const render = () => hooks.render(SessionPage, props);
  const driverBar = (tree) => nodes(tree).find((node) => node.type?.name === "DriverBar");
  const optionsOf = (tree) => nodes(tree).find((node) => node.props?.id === "takeover-options");
  let tree = render();
  assert.equal(optionsOf(tree), undefined, "the small settings panel starts closed");
  driverBar(tree).props.onOptions();
  tree = render();
  let options = optionsOf(tree);
  assert.ok(options);
  assert.equal(options.props.role, "region", "expandable takeover settings are a nonmodal region");
  assert.equal(options.props["aria-label"], "takeover_options");
  assert.ok(!nodes(tree).some((node) => node.props?.role === "dialog"), "the settings do not activate the conversation's dialog Escape guard");
  assert.ok(nodes(tree).some((node) => node.props?.className === "nt-stage" && nodes(node).includes(options)), "settings belong to the stage");
  assert.ok(!nodes(options).some((node) => node.type === AgentChat));
  nodes(options).find((node) => node.type === "input").props.onChange({ target: { checked: true } });
  nodes(options).find((node) => node.type === "textarea").props.onChange({ target: { value: "Changed the export folder." } });
  tree = render();
  options = optionsOf(tree);
  assert.equal(nodes(options).find((node) => node.type === "input").props.checked, true);
  assert.equal(nodes(options).find((node) => node.type === "textarea").props.value, "Changed the export folder.");
  assert.deepEqual(plain(calls[0]), ["test-host", "take_over", { agentMayWatch: true }]);
  let stopped = false;
  options.props.onKeyDown({ key: "Escape", preventDefault() {}, stopPropagation() { stopped = true; } });
  tree = render();
  assert.equal(optionsOf(tree), undefined, "Escape inside the region still collapses it");
  assert.equal(stopped, true, "the settings consume their own Escape");
  driverBar(tree).props.onOptions();
  tree = render();
  assert.equal(nodes(optionsOf(tree)).find((node) => node.type === "textarea").props.value, "Changed the export folder.", "collapsing settings preserves the hand-back draft");
  await driverBar(tree).props.onAction("hand_back");
  assert.deepEqual(plain(calls[1]), ["test-host", "hand_back", { note: "Changed the export folder." }]);
  tree = render();
  assert.equal(optionsOf(tree), undefined, "hand-back closes the panel");
  c.feed.driver = { state: "agent" };
  tree = render();
  const bar = driverBar(tree);
  assert.ok(!nodes(bar.type(bar.props)).some((node) => node.props?.["aria-controls"] === "takeover-options"), "only a human driver gets the settings entry");
  hooks.close();
});

test("a human driver keeps a tab that brings the hidden bars back, under the raised bottom bar", () => {
  const hooks = hooksFixture();
  const { SessionPage } = sessionModule(hooks, { kind: "tauri" }, () => assert.fail("nested Stream should not execute in the page fixture"));
  const c = { host: { name: "test-host", path: "direct" }, os: "windows", online: true, feed: {
    agent: null, pending: [], sessions: {}, steps: [], turns: [], task: null,
    driver: { state: "person", device: "test-device", since_ms: 1000, agent_may_watch: false },
  } };
  const props = { c, device: "test-device", waiting: 0, onBack() {}, onNav() {} };
  const handleOf = (tree) => nodes(tree).find((node) => node.props?.className === "nt-handle");
  const bottomOf = (tree) => nodes(tree).find((node) => String(node.props?.className).startsWith("nt-anchor bottom"));
  let tree = hooks.render(SessionPage, props);
  // The tab never leaves while you drive, so a click aimed at it cannot fall through to the remote.
  assert.ok(handleOf(tree), "the tab is there while the bars show");
  assert.equal(typeof handleOf(tree).props.onClick, "function");
  assert.equal(typeof handleOf(tree).props.onMouseEnter, "function");
  assert.equal(bottomOf(tree).props.className, "nt-anchor bottom raised");
  // Nothing hides the local cursor on the stage (black edges, the tab, "video ended").
  assert.ok(nodes(tree).every((node) => !String(node.props?.className ?? "").includes("hidecursor")));
  c.feed.driver = { state: "agent" }; tree = hooks.render(SessionPage, props);
  assert.ok(!handleOf(tree), "no tab when you are not driving");
  assert.equal(bottomOf(tree).props.className, "nt-anchor bottom");
  hooks.close();
});

// ---- The host's monitors: the picker, the stream it restarts, and the pointer on another monitor.

/** win-dev's monitors as its `system_info` lists them (2026-10-22): a portrait one right of the
 *  primary, one left of it, then the 5120x2160 primary; both portrait ones start 9 px higher. */
const THREE_MONITORS = [
  { index: 0, x: 5120, y: -9, width: 1440, height: 2560, primary: false },
  { index: 1, x: -1440, y: -9, width: 1440, height: 2560, primary: false },
  { index: 2, x: 0, y: 0, width: 5120, height: 2160, primary: true },
];

function catalogT(lang) {
  const { en } = module("../apps/desktop/src/lib/strings/en.ts");
  const catalog = lang === "zh" ? module("../apps/desktop/src/lib/strings/zh.ts").zh : en;
  return (key, vars = {}) => (catalog[key] ?? key).replace(/\{(\w+)\}/g, (_, k) => String(vars[k] ?? `{${k}}`));
}

function monitorsModule(storage = new Map()) {
  return module("../apps/desktop/src/lib/monitors.ts", {}, {
    localStorage: { getItem: (key) => storage.get(key) ?? null, setItem: (key, value) => storage.set(key, value) },
  });
}

test("monitors are named by where they stand next to the primary one and listed left to right", () => {
  const m = monitorsModule();
  assert.deepEqual(plain(m.pickerOrder(THREE_MONITORS).map((x) => x.index)), [1, 2, 0]);
  const zh = m.monitorNames(THREE_MONITORS, catalogT("zh"));
  assert.deepEqual([0, 1, 2].map((i) => m.monitorOption(THREE_MONITORS[i], zh)),
    ["右侧竖屏 · 1440×2560", "左侧竖屏 · 1440×2560", "主屏 · 5120×2160"]);
  const en = m.monitorNames(THREE_MONITORS, catalogT("en"));
  assert.deepEqual([0, 1, 2].map((i) => en.get(i)), ["Right portrait display", "Left portrait display", "Main display"]);
  // Above and below, and two on the same side numbered left to right.
  const stacked = [
    { index: 0, x: 0, y: 0, width: 1920, height: 1080, primary: true },
    { index: 1, x: 0, y: -1440, width: 2560, height: 1440, primary: false },
    { index: 2, x: 3840, y: 0, width: 1920, height: 1080, primary: false },
    { index: 3, x: 1920, y: 0, width: 1920, height: 1080, primary: false },
  ];
  const names = m.monitorNames(stacked, catalogT("zh"));
  assert.deepEqual([0, 1, 2, 3].map((i) => names.get(i)), ["主屏", "上方屏幕", "右侧屏幕 2", "右侧屏幕 1"]);
  // A host from before positions: numbers (portrait ones said so), the primary still named.
  const old = [{ index: 0, width: 1920, height: 1080, primary: false }, { index: 1, width: 2560, height: 1440, primary: true },
    { index: 2, width: 1440, height: 2560, primary: false }];
  assert.deepEqual([0, 1, 2].map((i) => m.monitorNames(old, catalogT("en")).get(i)), ["Display 1", "Main display", "Portrait display 3"]);
  assert.equal(m.monitorNames(old, catalogT("zh")).get(2), "竖屏 3");
  assert.deepEqual(plain(m.pickerOrder(old).map((x) => x.index)), [0, 1, 2]);
});

test("the monitor picked is remembered per computer, and a host's refusal of a gone monitor is recognised", () => {
  const storage = new Map();
  const m = monitorsModule(storage);
  assert.equal(m.loadMonitor("win-dev"), null);
  m.saveMonitor("win-dev", 2);
  m.saveMonitor("mac", 0);
  assert.equal(m.loadMonitor("win-dev"), 2);
  assert.equal(m.loadMonitor("mac"), 0);
  m.saveMonitor("win-dev", null);
  assert.equal(m.loadMonitor("win-dev"), null);
  assert.equal(m.loadMonitor("mac"), 0);
  storage.set("ai-remote.stream-monitor", "not json");
  assert.equal(m.loadMonitor("mac"), null);
  storage.set("ai-remote.stream-monitor", '{"mac":-1,"x":"2"}');
  assert.equal(m.loadMonitor("mac"), null);
  assert.equal(m.loadMonitor("x"), null);
  // This host's words, and older Windows and macOS hosts'.
  for (const reason of ["host 'win-dev': there is no monitor 3 on this computer: it has 3 (0 to 2)",
    "host 'win-dev': no such monitor: monitor not found", "host 'mac': monitor not found"]) {
    assert.equal(m.isMissingMonitor(reason), true, reason);
  }
  assert.equal(m.isMissingMonitor("host 'mac': screen capture permission was refused"), false);
});

test("the request names the monitor, and the reply's monitors and streamed index reach the page", async () => {
  const s = apiFixture({ screen: { x: -1440, y: -9, width: 1440, height: 2560 }, monitors: THREE_MONITORS, monitor: 1 });
  const view = await s.api.watchScreen("test-host", () => {}, { monitor: 1 });
  assert.equal(s.requests[0].request.monitor, 1);
  assert.deepEqual(plain(view.monitors), THREE_MONITORS);
  assert.equal(view.monitor, 1);
  assert.deepEqual(plain(view.screen), { x: -1440, y: -9, width: 1440, height: 2560 });
  // An older host lists monitors without positions and does not say which one it streams.
  const old = apiFixture({ screen: { x: 0, y: 0, width: 1600, height: 1000 }, monitors: [{ index: 0, width: 1600, height: 1000, primary: true }] });
  const oldView = await old.api.watchScreen("test-host", () => {});
  assert.equal(oldView.monitor, null);
  assert.equal(oldView.monitors.length, 1);
});

test("switching the monitor restarts the stream on it, and a gone monitor sends the page back to the primary one", async () => {
  const s = streamFixture();
  const reported = [];
  let missing = 0;
  s.update({ monitor: 2, onMonitors: (list, index) => reported.push([list.length, index]), onMonitorMissing: () => missing++ });
  s.render();
  assert.equal(s.views[0].options.monitor, 2);
  s.views[0].resolve(Object.assign(s.views[0].view, { monitors: THREE_MONITORS, monitor: 2 })); await tick();
  assert.deepEqual(reported, [[3, 2]]);
  s.decoders[0].onSize(1440, 2560);
  assert.equal(s.hasFrame(), true);
  s.update({ monitor: 1 }); s.render();
  assert.equal(s.views.length, 2, "a new monitor is a new stream");
  assert.equal(s.views[0].view.stops, 1);
  assert.equal(s.views[1].options.monitor, 1);
  assert.equal(s.hasFrame(), false);
  assert.equal(s.screens.at(-1), null, "no input lands on the old monitor's geometry meanwhile");
  // An older host does not name the monitor: the one asked for is the one streamed.
  s.views[1].resolve(Object.assign(s.views[1].view, { monitors: THREE_MONITORS, monitor: null })); await tick();
  assert.deepEqual(reported.at(-1), [3, 1]);
  // Unplugged since it was picked: the host refuses it; the page is told to fall back.
  s.update({ monitor: 3 }); s.render();
  s.views[2].reject(new Error("host 'test-host': there is no monitor 3 on this computer: it has 3 (0 to 2)")); await tick();
  assert.equal(missing, 1);
  // Any other refusal keeps the choice (a reconnect may well work).
  s.update({ monitor: 2 }); s.render();
  s.views[3].reject(new Error("host 'test-host': did not answer")); await tick();
  assert.equal(missing, 1);
  // The primary one is never "gone".
  s.update({ monitor: null }); s.render();
  s.views[4].reject(new Error("host 'test-host': monitor not found")); await tick();
  assert.equal(missing, 1);
  s.close();
});

test("the session page offers every monitor in the title bar and in full screen, and remembers the choice", () => {
  const hooks = hooksFixture();
  const storage = new Map([["ai-remote.stream-monitor", '{"other-host":2}']]);
  const { SessionPage, Stream, RemoteInput, TitleBar } = sessionModule(hooks, { kind: "tauri" },
    () => assert.fail("nested Stream should not execute in the page fixture"), storage);
  const c = { host: { name: "test-host", path: "direct" }, os: "windows", online: true, feed: {
    agent: null, pending: [], sessions: {}, steps: [], turns: [], task: null,
    driver: { state: "person", device: "test-device", since_ms: 1000, agent_may_watch: false },
  } };
  const props = { c, device: "test-device", waiting: 0, onBack() {}, onNav() {} };
  const render = () => hooks.render(SessionPage, props);
  const stream = (tree) => nodes(tree).find((node) => node.type === Stream);
  const titlePicker = (tree) => nodes(nodes(tree).find((node) => node.type === TitleBar).props.end)
    .find((node) => node.type === "select" && node.props["aria-label"] === "stream_monitor");
  const topPicker = (tree) => nodes(nodes(tree).find((node) => node.props?.className === "nt-anchor top"))
    .find((node) => node.type === "select" && node.props["aria-label"] === "stream_monitor");
  let tree = render();
  assert.equal(stream(tree).props.monitor, null, "another computer's choice is not this one's");
  assert.equal(titlePicker(tree), undefined, "nothing to pick before the host lists its monitors");
  stream(tree).props.onMonitors([THREE_MONITORS[2]], 2); tree = render();
  assert.equal(titlePicker(tree), undefined, "one monitor: no picker");
  stream(tree).props.onMonitors(THREE_MONITORS, 2);
  stream(tree).props.onFrame(); stream(tree).props.onScreen({ x: 0, y: 0, width: 5120, height: 2160 }); tree = render();
  const picker = titlePicker(tree);
  assert.equal(picker.props.value, 2, "the streamed monitor is the one shown");
  const options = nodes(picker).filter((node) => node.type === "option");
  assert.deepEqual(options.map((o) => o.props.value), [1, 2, 0], "left to right");
  assert.ok(nodes(tree).some((node) => node.type === RemoteInput));
  // Pick the left portrait monitor: a new stream there, no input until its frame and geometry arrive.
  let blurred = 0;
  picker.props.onChange({ currentTarget: { blur: () => blurred++ }, target: { value: "1" } }); tree = render();
  assert.equal(blurred, 1);
  assert.equal(stream(tree).props.monitor, 1);
  assert.equal(titlePicker(tree).props.value, 1, "the choice shows while the new stream starts");
  assert.equal(JSON.parse(storage.get("ai-remote.stream-monitor"))["test-host"], 1);
  assert.equal(JSON.parse(storage.get("ai-remote.stream-monitor"))["other-host"], 2);
  assert.ok(!nodes(tree).some((node) => node.type === RemoteInput), "the old monitor's geometry is dropped");
  stream(tree).props.onFrame(); stream(tree).props.onScreen({ x: -1440, y: -9, width: 1440, height: 2560 }); tree = render();
  assert.deepEqual(plain(nodes(tree).find((node) => node.type === RemoteInput).props.screen), { x: -1440, y: -9, width: 1440, height: 2560 });
  // Full screen keeps the picker in its top bar.
  nodes(nodes(tree).find((node) => node.type === TitleBar).props.end).find((node) => node.props?.["aria-label"] === "fs_enter").props.onClick();
  tree = render();
  assert.equal(topPicker(tree).props.value, 1);
  // The host no longer has it: back to the primary one, and forgotten.
  stream(tree).props.onMonitorMissing(); tree = render();
  assert.equal(stream(tree).props.monitor, null);
  assert.ok(nodes(tree).some((node) => node.props?.children === "monitor_gone"), "a note says why the picture changed");
  assert.equal(JSON.parse(storage.get("ai-remote.stream-monitor"))["test-host"], undefined);
  hooks.close();
  // Opening the computer again starts on the monitor picked last.
  const again = hooksFixture();
  storage.set("ai-remote.stream-monitor", '{"test-host":2}');
  const page = sessionModule(again, { kind: "tauri" }, () => assert.fail("nested Stream should not execute"), storage);
  tree = again.render(page.SessionPage, props);
  assert.equal(nodes(tree).find((node) => node.type === page.Stream).props.monitor, 2);
  again.close();
});

test("the host's pointer is placed on a monitor left of the primary one and hides once it leaves it", () => {
  const { createPointerLayer } = module("../apps/desktop/src/lib/pointer.ts");
  const noop = () => {};
  const ctx = { beginPath: noop, moveTo: noop, lineTo: noop, closePath: noop, fill: noop, stroke: noop, clearRect: noop, putImageData: noop };
  const layer = { style: {}, width: 0, height: 0, getContext: () => ctx };
  // A 1440x2560 portrait monitor shown 360 px wide: a quarter of its pixels.
  const video = { clientWidth: 360, offsetLeft: 0, offsetTop: 0 };
  const pointer = createPointerLayer(layer, video);
  const screen = { x: -1440, y: -9, width: 1440, height: 2560 };
  // Positions are relative to the streamed monitor (the host subtracts its origin).
  pointer.move({ x: 440, y: 109, visible: true, shape: 0 }, screen);
  assert.equal(layer.style.display, "block");
  assert.equal(layer.style.transform, "translate(110px, 27.25px)");
  // On the primary monitor (x 1450 here) or right of the left edge's negative side: not on this one.
  for (const at of [{ x: 1450, y: 19 }, { x: -1, y: 100 }, { x: 10, y: 2560 }]) {
    pointer.move({ ...at, visible: true, shape: 0 }, screen);
    assert.equal(layer.style.display, "none", JSON.stringify(at));
  }
  pointer.move({ x: 0, y: 0, visible: true, shape: 0 }, screen);
  assert.equal(layer.style.display, "block");
});
