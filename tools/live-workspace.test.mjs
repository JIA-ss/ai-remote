// The complete workspace when it shares the window with a live screen. These fixtures run the
// actual React component's hooks and handlers; they do not claim decoded video or browser layout.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

const jsx = (type, props, key) => ({ type, props, key });
const runtime = { jsx, jsxs: jsx, Fragment: Symbol("Fragment") };
const plain = (x) => JSON.parse(JSON.stringify(x));
const nodes = (tree) => !tree || typeof tree !== "object" ? [] : [tree, ...[tree.props?.children].flat(10).flatMap(nodes)];

function compiled(path, dependencies, globals = {}, expose = "") {
  const source = readFileSync(new URL(`../apps/desktop/src/${path}`, import.meta.url), "utf8") + expose;
  const text = ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022,
  } }).outputText;
  const exports = {};
  vm.runInNewContext(text, { exports, require(name) {
    assert.ok(name in dependencies, `unexpected dependency ${name}`);
    return dependencies[name];
  }, console, setTimeout, clearTimeout, Map, Set, Date, requestAnimationFrame: () => 1, cancelAnimationFrame() {}, ...globals }, { filename: path });
  return exports;
}

function hooks() {
  const values = [], pending = [];
  let index = 0;
  const effect = (run, deps) => {
    const at = index++, previous = values[at];
    if (previous && deps && previous.deps && deps.length === previous.deps.length && deps.every((x, i) => x === previous.deps[i])) return;
    pending.push(() => { previous?.cleanup?.(); values[at] = { deps, cleanup: run() }; });
  };
  const react = {
    useState(initial) {
      const at = index++;
      if (!(at in values)) values[at] = typeof initial === "function" ? initial() : initial;
      return [values[at], (value) => { values[at] = typeof value === "function" ? value(values[at]) : value; }];
    },
    useRef(initial) {
      const at = index++;
      if (!(at in values)) values[at] = { current: initial };
      return values[at];
    },
    useCallback: (fn) => fn,
    useMemo: (fn) => fn(),
    useEffect: effect, useLayoutEffect: effect,
  };
  return {
    react,
    render(Component, props, commit = () => {}) {
      index = 0;
      const tree = Component(props);
      commit(tree);
      for (const run of pending.splice(0)) run();
      return tree;
    },
    close() { for (const value of values) value?.cleanup?.(); },
  };
}

test("a new popup menu focuses its chosen row only after its measured position is committed visible", () => {
  const h = hooks(), focuses = [];
  let style = {}, closed = 0;
  const body = {}, first = { focus() { focuses.push({ row: "first", visible: style.visibility !== "hidden" });
    if (style.visibility !== "hidden") document.activeElement = first; } };
  const selected = { focus() { focuses.push({ row: "selected", visible: style.visibility !== "hidden" });
    if (style.visibility !== "hidden") document.activeElement = selected; } };
  const document = { body, activeElement: body, documentElement: { clientWidth: 1280, clientHeight: 800 }, addEventListener() {}, removeEventListener() {} };
  const element = { getBoundingClientRect: () => ({ width: 220, height: 150 }),
    querySelector: (selector) => selector.includes("data-autofocus") ? null : selector.includes("aria-checked=true") ? selected : first,
    querySelectorAll: () => [first, selected], contains: (node) => node === first || node === selected };
  const { PopMenu } = compiled("ui/workspace/parts.tsx", {
    react: h.react, "react/jsx-runtime": runtime, "../../lib/i18n": { t: (key) => key }, "../../lib/platform": { detectOs: () => "mac" },
    "../util": { cx: (...xs) => xs.filter(Boolean).join(" ") },
    "lucide-react": Object.fromEntries(["ArrowLeft", "ArrowRight", "Check", "Laptop", "LaptopMinimal", "Monitor", "PanelLeft"].map((name) => [name, () => null])),
  }, { document, HTMLElement: Element, ResizeObserver: class { observe() {} disconnect() {} },
    requestAnimationFrame: () => 1, cancelAnimationFrame() {}, window: { addEventListener() {}, removeEventListener() {} } });
  const props = { at: { x: 30, y: 50 }, label: "Session actions", onClose: () => { closed++; },
    items: [{ key: "first", label: "First", run() {} }, { key: "selected", label: "Selected", checked: true, run() {} }] };
  const render = () => h.render(PopMenu, props, (tree) => { tree.props.ref.current = element; style = tree.props.style; });
  render();
  assert.equal(style.visibility, "hidden", "the first commit exists only to measure the menu");
  assert.deepEqual(focuses, [], "focus on visibility:hidden is ignored by the native webview and must not be attempted");
  assert.equal(document.activeElement, body);
  let tree = render();
  assert.deepEqual(plain(style), { left: 30, top: 50 });
  assert.deepEqual(focuses, [{ row: "selected", visible: true }], "the chosen enabled row receives focus in the visible commit");
  assert.equal(document.activeElement, selected);
  tree = render();
  assert.equal(focuses.length, 1, "unrelated renders do not steal menu focus again");
  const input = { tagName: "INPUT" };
  document.activeElement = input;
  props.items = props.items.map((item) => ({ ...item, label: `Confirm ${item.label}`, keep: true }));
  tree = render();
  assert.equal(focuses.length, 1, "retained-menu confirmation updates do not refocus its first or checked item");
  assert.equal(document.activeElement, input, "a field the person focused keeps its focus across item updates");
  const key = { key: "Escape", nativeEvent: {}, preventDefault() { this.prevented = true; }, stopPropagation() { this.stopped = true; } };
  tree.props.onKeyDown(key);
  assert.equal(closed, 1);
  assert.equal(key.prevented, true);
  assert.equal(key.stopped, true, "Escape is consumed by the now-focused menu before chat can arm Stop");
  h.close();
});

class Node {}
class Element extends Node {
  constructor(parent = null) { super(); this.parent = parent; this.tagName = "DIV"; this.clientWidth = 800; this.dataset = {}; }
  contains(node) { for (let el = node; el; el = el.parent) if (el === this) return true; return false; }
  addEventListener() {}
  removeEventListener() {}
  getBoundingClientRect() { return { left: 900, top: 0, width: this.clientWidth, bottom: 800 }; }
  querySelector() { return null; }
  closest(selector) {
    if (selector === "[inert]") for (let at = this; at; at = at.parent) if (at.inert) return at;
    if (selector === ".gl-modal-root") for (let at = this; at; at = at.parent) if (at.modalRoot) return at;
    return null;
  }
}

function workspaceFixture({ width = 660, embedded = true, route = {}, initialTarget = null } = {}) {
  const h = hooks(), listeners = new Map(), documentListeners = new Map(), observers = [], calls = [], storage = new Map();
  const root = new Element(); root.clientWidth = width;
  const inside = new Element(root), remote = new Element();
  const body = new Element(), html = new Element(); body.tagName = "BODY"; html.tagName = "HTML"; html.clientWidth = 1800;
  const add = (map, name, fn) => { if (!map.has(name)) map.set(name, new Set()); map.get(name).add(fn); };
  const remove = (map, name, fn) => map.get(name)?.delete(fn);
  const document = { activeElement: inside, body, documentElement: html, querySelector: () => null,
    addEventListener: (name, fn) => add(documentListeners, name, fn), removeEventListener: (name, fn) => remove(documentListeners, name, fn) };
  const window = { innerWidth: 1800, addEventListener: (name, fn) => add(listeners, name, fn), removeEventListener: (name, fn) => remove(listeners, name, fn) };
  const NewSession = () => null, SessionPage = () => null, Sidebar = () => null, Palette = () => null;
  const data = compiled("lib/workspace.ts", { "./chat": compiled("lib/chat.ts", {}, { localStorage: { getItem: () => null } }) }, { localStorage: { getItem: () => null } });
  const parts = compiled("ui/workspace/parts.tsx", {
    react: h.react, "react/jsx-runtime": runtime, "../../lib/i18n": { t: (key) => key }, "../../lib/platform": { detectOs: () => "mac" },
    "../util": { cx: (...xs) => xs.filter(Boolean).join(" ") },
    "lucide-react": Object.fromEntries(["ArrowLeft", "ArrowRight", "Check", "Laptop", "LaptopMinimal", "Monitor", "PanelLeft"].map((name) => [name, () => null])),
  }, { Node, Element, HTMLElement: Element, document, window });
  const { Workspace } = compiled("ui/workspace/Workspace.tsx", {
    react: h.react, "react/jsx-runtime": runtime,
    "../../lib/i18n": { t: (key) => key }, "../../lib/workspace": data,
    "../util": { cx: (...xs) => xs.filter(Boolean).join(" ") },
    "./NewSession": { NewSession }, "./SessionPage": { SessionPage }, "./Sidebar": { Sidebar }, "./Palette": { Palette },
    "./parts": parts,
    "./actions": { archive() {}, fork() {}, markDone() {}, togglePin() {}, toggleRead() {}, unarchive() {} },
    "./FamilyConfirm": { FamilyConfirm: () => null },
    "./useWorkspace": { useWorkspace: () => ({ items: [], loaded: true }), workspace: { items: () => [], dot: () => null } },
  }, {
    Node, Element, HTMLElement: Element, HTMLInputElement: class extends Element {},
    ResizeObserver: class {
      constructor(fn) { this.fn = fn; observers.push(this); }
      observe(el) { this.el = el; }
      disconnect() { this.disconnected = true; }
    },
    localStorage: { getItem: (k) => storage.get(k) ?? null, setItem: (k, v) => storage.set(k, v) },
    window, document,
  });
  let props = { computers: [], device: "test-device", route, initialTarget, embedded,
    onRoute: (r) => calls.push(plain(r)), back: null, forward: null, onMode() {}, onWatch() {}, onPair() {}, onSettings() {} };
  const render = () => h.render(Workspace, props, (tree) => { tree.props.ref.current = root; });
  const key = (target, value, mods = {}) => {
    const e = { target, key: value, code: `Key${value.toUpperCase()}`, metaKey: true, ctrlKey: false, shiftKey: false, altKey: false,
      defaultPrevented: false, isComposing: false, preventDefault() { this.defaultPrevented = true; }, ...mods };
    for (const fn of [...(listeners.get("keydown") ?? [])]) fn(e);
    return e;
  };
  const documentEvent = (name, target) => { for (const fn of [...(documentListeners.get(name) ?? [])]) fn({ target, type: name }); };
  return { render, root, inside, remote, body, document, documentEvent, documentListeners, calls, observers, key, NewSession, SessionPage, Sidebar, Palette,
    update(next) { props = { ...props, ...next }; }, close: h.close };
}

function liveFixture() {
  const h = hooks(), storage = new Map(), opened = [];
  const nav = compiled("lib/nav.ts", {}, { URLSearchParams });
  const SessionPage = () => null, Workspace = () => null, DecisionCard = () => null;
  const { LiveWorkspace } = compiled("ui/LiveWorkspace.tsx", {
    react: h.react, "react/jsx-runtime": runtime,
    "../lib/chat": { lastCwd: (host) => `/${host}/project` },
    "../lib/i18n": { t: (key) => key }, "../lib/nav": nav,
    "./Session": { SessionPage }, "./workspace/Workspace": { Workspace }, "./Decide": { DecisionCard },
    "./util": { agentLabel: () => "Agent", cx: (...xs) => xs.filter(Boolean).join(" ") },
  }, { localStorage: { getItem: (key) => storage.get(key) ?? null, setItem: (key, value) => storage.set(key, value) } });
  const computer = (name) => ({ host: { name }, online: true, feed: { pending: [], steps: [], agent: null } });
  const computers = [computer("a"), computer("b")];
  let route = nav.screenRoute("a", "one"), extra = {};
  const props = () => ({ c: computers.find((c) => c.host.name === route.host), computers, route, device: "test-device", waiting: 0,
    onRoute(location, replace) { opened.push({ location: plain(location), replace }); route = nav.screenWorkspaceRoute(route, location); },
    back() {}, forward() {}, onNav() {}, onMode() {}, onPair() {}, onSettings() {}, ...extra });
  const root = new Element();
  return { render: () => h.render(LiveWorkspace, props(), (tree) => { tree.props.ref.current = root; }),
    SessionPage, Workspace, DecisionCard, computers, opened, update(value) { extra = { ...extra, ...value }; }, close: h.close };
}

test("the complete workspace and its screen are stable siblings while conversations and projects change", () => {
  const f = liveFixture(); let tree = f.render();
  const screen = () => nodes(tree).find((n) => n.type === f.SessionPage);
  const workspace = () => nodes(tree).find((n) => n.type === f.Workspace);
  const initialScreen = screen(), initialWorkspace = workspace();
  assert.equal(nodes(tree).filter((n) => n.type === f.SessionPage).length, 1);
  assert.equal(nodes(tree).filter((n) => n.type === f.Workspace).length, 1);
  assert.equal(initialWorkspace.props.embedded, true);
  assert.equal(initialScreen.key, "a");
  assert.deepEqual(plain(initialWorkspace.props.route), { host: "a", session: "one" });
  assert.ok(!nodes(tree).some((n) => n.props?.role === "tablist" || n.props?.className === "nt-chat-picker"));
  for (const location of [{ host: "a", session: "two" }, { place: { host: "a", cwd: "/another-project" } }, { host: "a", session: "one" }]) {
    workspace().props.onRoute(location); tree = f.render();
    assert.equal(screen().type, initialScreen.type);
    assert.equal(screen().key, initialScreen.key, "same-computer session and project changes preserve the screen mount");
    assert.equal(workspace().type, initialWorkspace.type);
    assert.equal(workspace().key, initialWorkspace.key, "the full workspace also stays mounted");
  }
  workspace().props.onRoute({ host: "b", session: "other" }); tree = f.render();
  assert.equal(screen().key, "b", "only the screen remounts for the other computer");
  assert.equal(screen().props.c.host.name, "b");
  assert.equal(workspace().type, initialWorkspace.type);
  assert.equal(workspace().key, initialWorkspace.key);
  workspace().props.onPlaceChange({ host: "a", cwd: "/new" }); tree = f.render();
  assert.equal(screen().key, "a");
  assert.deepEqual(f.opened.at(-1), { location: { place: { host: "a", cwd: "/new" } }, replace: true },
    "choosing the new session's environment updates the picture without adding folder history");
  f.close();
});

test("host approval stays above the workspace and immersive mode retains both components", () => {
  const f = liveFixture();
  f.computers[0].feed.pending = [{ id: "approval-one", tool: "shell" }];
  f.computers[0].feed.steps = [{ approvalId: "approval-one", startedMs: 123 }];
  let tree = f.render();
  const screen = nodes(tree).find((n) => n.type === f.SessionPage);
  const approval = nodes(tree).find((n) => n.type === f.DecisionCard);
  assert.equal(approval.props.p.id, "approval-one");
  assert.equal(approval.props.askedMs, 123);
  assert.equal(approval.props.escapeScope, screen.props.approvalRef, "Review focuses the same wrapper that owns Escape");
  assert.equal(screen.props.approvalVisible, true);
  assert.ok(nodes(tree).indexOf(approval) < nodes(tree).findIndex((n) => n.type === f.Workspace));
  screen.props.onImmersiveChange(true); tree = f.render();
  assert.match(tree.props.className, /immersive/);
  assert.equal(nodes(tree).find((n) => n.type === "aside").props.inert, true);
  assert.equal(nodes(tree).find((n) => n.type === f.SessionPage).props.approvalVisible, false,
    "the screen waits to focus approvals while their parent pane is inert");
  assert.equal(nodes(tree).filter((n) => n.type === f.SessionPage).length, 1);
  assert.equal(nodes(tree).filter((n) => n.type === f.Workspace).length, 1,
    "full screen hides the existing workspace without destroying its draft or ticket state");
  nodes(tree).find((n) => n.type === f.SessionPage).props.onImmersiveChange(false); tree = f.render();
  assert.equal(nodes(tree).find((n) => n.type === "aside").props.inert, false);
  assert.equal(nodes(tree).find((n) => n.type === f.SessionPage).props.approvalVisible, true);
  f.close();
});

test("an inactive window forwards capture suspension while keeping its workspace mounted", () => {
  const f = liveFixture(); let tree = f.render();
  const initial = nodes(tree).find((n) => n.type === f.Workspace);
  assert.equal(nodes(tree).find((n) => n.type === f.SessionPage).props.active, true);
  f.update({ active: false }); tree = f.render();
  assert.equal(nodes(tree).find((n) => n.type === f.SessionPage).props.active, false);
  const kept = nodes(tree).find((n) => n.type === f.Workspace);
  assert.equal(kept.type, initial.type);
  assert.equal(kept.key, initial.key);
  assert.deepEqual(plain(kept.props.route), plain(initial.props.route));
  f.close();
});

test("an embedded workspace folds its sidebar at its own width, and observes the pane resizing", () => {
  const f = workspaceFixture({ width: 660 });
  f.render();
  let tree = f.render();
  assert.equal(tree.props["data-side"], "hidden", "a wide window does not dock a sidebar inside a narrow pane");
  assert.equal(f.observers.length, 1);
  assert.equal(f.observers[0].el, f.root);
  f.key(f.inside, "b"); tree = f.render();
  assert.equal(tree.props["data-side"], "float", "the existing sidebar opens over the conversation");
  f.root.clientWidth = 960; f.observers[0].fn(); tree = f.render();
  assert.equal(tree.props["data-side"], "docked");
  assert.ok(nodes(tree).some((n) => n.type === f.Sidebar));
  f.close();
  assert.equal(f.observers[0].disconnected, true);
});

test("workspace shortcuts stay inside the embedded pane and leave remote input untouched", () => {
  const f = workspaceFixture(); f.render(); f.render();
  for (const key of ["n", "k", "b"]) {
    assert.equal(f.key(f.remote, key).defaultPrevented, false, `remote ${key} belongs to the computer`);
  }
  assert.deepEqual(f.calls, []);
  assert.ok(!nodes(f.render()).some((n) => n.type === f.Palette));
  assert.equal(f.key(f.inside, "n").defaultPrevented, true);
  assert.deepEqual(f.calls, [{ place: null }]);
  assert.equal(f.key(f.inside, "k").defaultPrevented, true);
  assert.ok(nodes(f.render()).some((n) => n.type === f.Palette));
  assert.equal(f.key(f.inside, "b", { isComposing: true }).defaultPrevented, false);
  f.close();
});

test("a removed right-pane control keeps workspace shortcuts on body until the person uses the screen", () => {
  const f = workspaceFixture(); f.render(); f.render();
  // The focused row or menu was removed by a normal workspace action. The browser falls back to
  // body; no test driver focuses a replacement control before the person's next shortcut.
  f.document.activeElement = f.body;
  f.documentEvent("focusin", f.body);
  assert.equal(f.key(f.body, "k").defaultPrevented, true, "Search still belongs to the pane used immediately before the fallback");
  assert.ok(nodes(f.render()).some((n) => n.type === f.Palette));
  nodes(f.render()).find((n) => n.type === f.Palette).props.onClose();
  f.render();
  f.documentEvent("pointerdown", f.remote);
  assert.equal(f.key(f.body, "n").defaultPrevented, false, "using the remote picture clears the workspace's body fallback ownership");
  assert.deepEqual(f.calls, []);
  f.document.activeElement = f.inside; f.documentEvent("focusin", f.inside);
  f.document.activeElement = f.body; f.documentEvent("focusin", f.body);
  assert.equal(f.key(f.body, "n").defaultPrevented, true);
  assert.deepEqual(f.calls, [{ place: null }]);
  // A real focused control outside the pane wins even if its focusin was not observed yet.
  f.document.activeElement = f.remote;
  assert.equal(f.key(f.body, "b").defaultPrevented, false);
  f.document.activeElement = f.body;
  f.documentEvent("pointerdown", f.body);
  assert.equal(f.key(f.body, "k").defaultPrevented, false, "an actual click on the page background is not a removed-control fallback");
  assert.equal(f.key(f.document, "n").defaultPrevented, false, "a synthetic document target does not claim pane ownership");
  f.close();
  assert.ok([...f.documentListeners.values()].every((listeners) => listeners.size === 0), "pane ownership listeners are removed on close");
});

test("an inert workspace never consumes shortcuts from its controls or a remembered body fallback", () => {
  const f = workspaceFixture(); f.render(); f.render();
  f.root.inert = true;
  assert.equal(f.key(f.inside, "k").defaultPrevented, false);
  f.document.activeElement = f.body; f.documentEvent("focusin", f.body);
  assert.equal(f.key(f.body, "n").defaultPrevented, false);
  assert.deepEqual(f.calls, []);
  assert.ok(!nodes(f.render()).some((n) => n.type === f.Palette));
  f.close();
});

test("the existing first-message flow keeps its first attachment ticket for the matching session only", () => {
  const f = workspaceFixture(); let tree = f.render();
  const create = nodes(tree).find((n) => n.type === f.NewSession);
  assert.equal(create.props.embedded, true);
  create.props.onStarted("test-host", "new-session", "first-ticket");
  assert.deepEqual(f.calls, [{ host: "test-host", session: "new-session" }], "history gets an id, never the one-use ticket");
  f.update({ route: f.calls[0] }); tree = f.render();
  assert.equal(nodes(tree).find((n) => n.type === f.SessionPage).props.ticket, "first-ticket");
  f.update({ route: { host: "other-host", session: "new-session" } }); tree = f.render();
  assert.equal(nodes(tree).find((n) => n.type === f.SessionPage).props.ticket, null, "equal ids on different computers do not share a ticket");
  f.close();

  const opened = workspaceFixture({ route: { host: "test-host", session: "started-elsewhere" },
    initialTarget: { host: "test-host", session: "started-elsewhere", ticket: "one-use" } });
  assert.equal(nodes(opened.render()).find((n) => n.type === opened.SessionPage).props.ticket, "one-use");
  opened.update({ route: { host: "test-host", session: "another-session" } });
  assert.equal(nodes(opened.render()).find((n) => n.type === opened.SessionPage).props.ticket, null);
  opened.close();
});

test("an opened workspace conversation stays mounted through offline reports, while first-open offline and access errors keep their page states", () => {
  function fixture({ online = true, status = "ok", found = true } = {}) {
    const h = hooks();
    const ClaudeConversation = () => null, CodexConversation = () => null;
    const computer = { host: { name: "test-host" }, online, since: Date.now() - 60_000, problem: null };
    const item = { host: "test-host", key: "test-host/session", local: false,
      meta: { id: "session", runtime: "claude", name: "Claude", title: "A session", cwd: "/project", state: "idle" } };
    const state = { item: found ? item : null };
    const snap = { hosts: { "test-host": { status, fetchedAt: Date.now() + 1000 } } };
    const workspace = { find: () => state.item, setCurrent() {}, refresh: async () => {} };
    const { SessionPage } = compiled("ui/workspace/SessionPage.tsx", {
      react: h.react, "react/jsx-runtime": runtime,
      "lucide-react": { MonitorPlay: () => null, MoreHorizontal: () => null },
      "../../lib/chat": { folderName: (path) => path.split("/").at(-1) },
      "../../lib/i18n": { t: (key) => key }, "../../lib/workspace": { sessionKey: (host, id) => `${host}/${id}` },
      "../claude/ClaudeConversation": { ClaudeConversation },
      "../codex/CodexConversation": { CodexConversation, isCodexRuntime: (runtime) => runtime.id === "codex" },
      "./parts": { Later: () => null, PopMenu: () => null },
      "./Sidebar": { RenameField: () => null, titleOf: (value) => value.meta.title },
      "./actions": {}, "./useWorkspace": { workspace },
    });
    const props = { snap, computers: [computer], host: "test-host", id: "session", ticket: "one-use", corner: null,
      renaming: false, setRenaming() {}, onWatch() {}, onLeave() {}, onArchived() {}, onNew() {}, toast() {}, embedded: true };
    return { render: () => h.render(SessionPage, props), computer, snap, state, ClaudeConversation, close: h.close };
  }
  const opened = fixture();
  const first = nodes(opened.render()).find((node) => node.type === opened.ClaudeConversation);
  assert.ok(first, "a valid online session starts the ordinary conversation component");
  opened.computer.online = false;
  const offline = nodes(opened.render()).find((node) => node.type === opened.ClaudeConversation);
  assert.equal(offline?.type, first.type);
  assert.equal(offline?.key, first.key, "the connection, draft and pending sends retain their component identity");
  assert.deepEqual(plain(offline.props.target), plain(first.props.target));
  assert.ok(!nodes(opened.render()).some((node) => node.props?.title === "ws_disconnected"));
  opened.computer.online = true;
  assert.equal(nodes(opened.render()).find((node) => node.type === opened.ClaudeConversation)?.key, first.key,
    "recovery continues the existing useConversation Retry path");
  for (const status of ["unsupported", "denied"]) {
    opened.snap.hosts["test-host"].status = status;
    const tree = opened.render();
    assert.ok(!nodes(tree).some((node) => node.type === opened.ClaudeConversation), `${status} still replaces the conversation`);
    assert.ok(nodes(tree).some((node) => node.props?.title === (status === "denied" ? "ws_denied" : "ws_too_old")));
  }
  opened.snap.hosts["test-host"].status = "ok"; opened.state.item = null;
  assert.ok(nodes(opened.render()).some((node) => node.props?.title === "ws_not_found"), "a genuinely missing session keeps its recovery page");
  opened.close();
  const initiallyOffline = fixture({ online: false });
  const offlinePage = initiallyOffline.render();
  assert.ok(!nodes(offlinePage).some((node) => node.type === initiallyOffline.ClaudeConversation));
  assert.ok(nodes(offlinePage).some((node) => node.props?.title === "ws_disconnected"), "opening an already offline computer still explains its offline state");
  initiallyOffline.close();
});

test("sidebar numeric jumps respect embedded ownership and consumed keys without changing standalone shortcuts", () => {
  const h = hooks(), listeners = new Map(), timers = new Map(), opened = [];
  let timerId = 0, embedded = true;
  const root = new Element(); root.dataset.keyboardActive = "true";
  const list = new Element(root), inside = new Element(root), remote = new Element();
  const body = new Element(), html = new Element();
  const document = { body, documentElement: html, activeElement: inside, hidden: false,
    addEventListener() {}, removeEventListener() {} };
  const rows = Array.from({ length: 3 }, (_, index) => {
    const owner = new Element(list); owner.dataset.rowKey = `test-host/session-${index + 1}`;
    const button = new Element(owner);
    button.getClientRects = () => [{}];
    button.closest = (selector) => selector === "[data-row-key]" ? owner : selector === "[inert], [aria-hidden=true]" && root.inert ? root : null;
    button.focus = () => { document.activeElement = button; };
    button.scrollIntoView = () => {};
    return button;
  });
  list.querySelectorAll = () => rows;
  list.closest = (selector) => selector === '.ws[data-embedded="true"]' && embedded ? root : null;
  const window = { devicePixelRatio: 2,
    addEventListener(name, listener) { if (!listeners.has(name)) listeners.set(name, new Set()); listeners.get(name).add(listener); },
    removeEventListener: (name, listener) => listeners.get(name)?.delete(listener),
    matchMedia: () => ({ addEventListener() {}, removeEventListener() {} }) };
  const data = compiled("lib/workspace.ts", { "./chat": compiled("lib/chat.ts", {}) });
  const parts = compiled("ui/workspace/parts.tsx", {
    react: h.react, "react/jsx-runtime": runtime, "../../lib/i18n": {}, "../../lib/platform": { detectOs: () => "mac" },
    "../util": {}, "lucide-react": {},
  }, { Node, document, window });
  const { testSessionJumps } = compiled("ui/workspace/Sidebar.tsx", {
    react: h.react, "react/jsx-runtime": runtime, "lucide-react": {}, "../../lib/chat": {},
    "../../lib/i18n": {}, "../../lib/workspace": data,
    "../../lib/workspace-batch": compiled("lib/workspace-batch.ts", { "./workspace": data }), "../util": {}, "./actions": {},
    "./ModeSwitch": {}, "./parts": parts, "./useWorkspace": {},
  }, { Node, HTMLElement: Element, window, document,
    setTimeout: (run) => { timers.set(++timerId, run); return timerId; }, clearTimeout: (id) => timers.delete(id),
    MutationObserver: class { observe() {} disconnect() {} },
  }, "\nexport const testSessionJumps = useSessionJumps;\n");
  const ref = { current: list };
  const render = () => h.render(() => testSessionJumps(ref, false, (key) => opened.push(key)), {});
  const press = (target, key, code = key, defaultPrevented = false) => {
    const event = { target, key, code, defaultPrevented, ctrlKey: true, shiftKey: true, metaKey: false, altKey: false,
      repeat: false, isComposing: false, preventDefault() { this.defaultPrevented = true; } };
    for (const listener of listeners.get("keydown") ?? []) listener(event);
    return event;
  };
  const hold = () => { const pending = [...timers.values()]; timers.clear(); for (const run of pending) run(); };
  render();
  press(remote, "Shift"); hold();
  assert.deepEqual(plain(render()), [], "remote modifier holds do not reveal workspace badges");
  assert.equal(press(remote, "!", "Digit1").defaultPrevented, false);
  assert.deepEqual(opened, [], "the remote computer keeps Ctrl+Shift+digits");
  press(inside, "Shift"); hold();
  assert.deepEqual(plain(render()), ["test-host/session-1", "test-host/session-2", "test-host/session-3"]);
  press(remote, "@", "Digit2");
  assert.deepEqual(plain(render()), [], "an external key removes held workspace hints");
  press(inside, "Shift");
  press(inside, "Shift", "Shift", true); hold();
  assert.deepEqual(plain(render()), [], "a consumed key cancels the hold instead of installing hints");
  assert.equal(press(inside, "@", "Digit2").defaultPrevented, true);
  assert.deepEqual(opened, ["test-host/session-2"], "an owned quick digit still jumps without waiting for hints");
  document.activeElement = body;
  press(body, "!", "Digit1");
  assert.deepEqual(opened, ["test-host/session-2", "test-host/session-1"], "logical right-pane body ownership also works for jumps");
  root.inert = true;
  assert.equal(press(inside, "!", "Digit1").defaultPrevented, false);
  root.inert = false; embedded = false;
  assert.equal(press(remote, "#", "Digit3").defaultPrevented, true, "standalone retains its existing window shortcuts");
  assert.deepEqual(opened, ["test-host/session-2", "test-host/session-1", "test-host/session-3"]);
  h.close();
  assert.ok([...listeners.values()].every((set) => set.size === 0));
});

test("sidebar multi-selection capture leaves external and inert panes' keys untouched", () => {
  const h = hooks(), listeners = new Map();
  let embedded = true;
  const root = new Element(); root.dataset.keyboardActive = "true";
  const side = new Element(root), list = new Element(side), inside = new Element(side), remote = new Element();
  const body = new Element(), html = new Element();
  side.closest = list.closest = (selector) => selector === '.ws[data-embedded="true"]' && embedded ? root : null;
  const document = { body, documentElement: html, activeElement: inside, hidden: false,
    addEventListener() {}, removeEventListener() {} };
  const window = { devicePixelRatio: 1,
    addEventListener(name, listener, capture = false) {
      if (!listeners.has(name)) listeners.set(name, new Map());
      listeners.get(name).set(listener, capture);
    },
    removeEventListener: (name, listener) => listeners.get(name)?.delete(listener),
    matchMedia: () => ({ addEventListener() {}, removeEventListener() {} }) };
  const data = compiled("lib/workspace.ts", { "./chat": compiled("lib/chat.ts", {}) });
  const batch = compiled("lib/workspace-batch.ts", { "./workspace": data });
  const items = ["one", "two"].map((id) => ({ host: "test-host", key: `test-host/${id}`, local: false, online: true,
    meta: { id, title: id, runtime: "claude", name: "Claude", cwd: "/fixture", state: "idle", started_at: 1, last_activity: 1 } }));
  const rowElements = items.map((item) => { const row = new Element(list); row.dataset.rowKey = item.key; return row; });
  list.querySelectorAll = (selector) => selector === "[data-row-key]" ? rowElements : [];
  const parts = compiled("ui/workspace/parts.tsx", {
    react: h.react, "react/jsx-runtime": runtime, "../../lib/i18n": {}, "../../lib/platform": { detectOs: () => "mac" },
    "../util": {}, "lucide-react": {},
  }, { Node, document, window });
  const { Sidebar } = compiled("ui/workspace/Sidebar.tsx", {
    react: h.react, "react/jsx-runtime": runtime, "lucide-react": {}, "../../lib/chat": { knownHome: () => null },
    "../../lib/i18n": { t: (key) => key, currentLang: () => "en" }, "../../lib/workspace": data, "../../lib/workspace-batch": batch,
    "../util": { cx: (...xs) => xs.filter(Boolean).join(" ") }, "./actions": {}, "./ModeSwitch": {}, "./parts": parts,
    "./useWorkspace": { workspace: { items: () => items, title: (item) => item.meta.title, isPinned: () => false,
      unread: () => false, dot: () => null, setArchived() {} } },
  }, { Node, Element, HTMLElement: Element, HTMLInputElement: class extends Element {}, window, document,
    localStorage: { getItem: () => null, setItem() {} }, MutationObserver: class { observe() {} disconnect() {} } });
  const props = { snap: { pins: {}, hosts: { "test-host": { status: "ok", update: true } } },
    computers: [{ host: { name: "test-host" }, online: true }], corner: null, mode: "agents", current: null,
    onMode() {}, onOpen() { assert.fail("selecting rows must not open a session"); }, onNew() {}, onPalette() {},
    onPair() {}, onSettings() {}, onArchived() {}, device: "fixture", toast() {}, renaming: null, setRenaming() {} };
  const render = () => h.render(Sidebar, props, (tree) => {
    tree.props.ref.current = side;
    nodes(tree).find((node) => node.props?.className === "ws-list").props.ref.current = list;
  });
  const rows = (tree) => nodes(tree).filter((node) => node.props?.item && typeof node.props.onOpen === "function");
  const selectBoth = () => {
    const tree = render();
    for (const row of rows(tree)) row.props.onOpen({ metaKey: false, ctrlKey: true, shiftKey: false, preventDefault() {} });
    assert.equal(rows(render()).filter((row) => row.props.multiSelected).length, 2);
  };
  const press = (target, key, mods = {}) => {
    const event = { target, key, code: key === "F2" || key === "Escape" ? key : `Key${key.toUpperCase()}`,
      defaultPrevented: false, isComposing: false, ctrlKey: false, metaKey: false, altKey: false, shiftKey: false,
      preventDefault() { this.defaultPrevented = true; }, stopPropagation() { this.stopped = true; }, ...mods };
    const handlers = [...(listeners.get("keydown") ?? [])].sort((a, b) => Number(b[1]) - Number(a[1]));
    for (const [listener] of handlers) { listener(event); if (event.stopped) break; }
    return event;
  };
  const chords = [["F2", {}], ["Escape", {}], ...["p", "u", "a", "r"].flatMap((key) =>
    [[key, { ctrlKey: true, altKey: true }], [key, { metaKey: true, altKey: true }]])];
  selectBoth();
  document.activeElement = remote;
  for (const [key, mods] of chords) {
    const event = press(remote, key, mods);
    assert.equal(event.defaultPrevented, false, `remote ${key} keeps its default`);
    assert.equal(event.stopped, undefined, `remote ${key} reaches the screen handler`);
  }
  assert.equal(press(body, "Escape").defaultPrevented, false, "a focused external control wins over stale body ownership");
  const sidebarRef = render().props.ref;
  sidebarRef.current = null;
  for (const key of ["F2", "Escape"]) {
    const event = press(remote, key);
    assert.equal(event.defaultPrevented, false, `a cleared sidebar ref leaves external ${key} untouched before effect cleanup`);
    assert.equal(event.stopped, undefined);
  }
  sidebarRef.current = side;
  document.activeElement = inside; root.inert = true;
  for (const [key, mods] of chords) {
    const event = press(inside, key, mods);
    assert.equal(event.defaultPrevented, false, `inert ${key} keeps its default`);
    assert.equal(event.stopped, undefined, `inert ${key} is not captured`);
  }
  root.inert = false;
  assert.equal(rows(render()).filter((row) => row.props.multiSelected).length, 2, "external and inert Escape did not mutate selection");
  assert.equal(press(inside, "Escape", { defaultPrevented: true }).stopped, undefined, "already-consumed keys are left alone");
  for (const [key, mods] of chords.filter(([key]) => key !== "Escape")) {
    const event = press(inside, key, mods);
    assert.equal(event.defaultPrevented, true, `owned ${key} still protects the multi-selection from single-row actions`);
    assert.equal(event.stopped, true);
  }
  assert.equal(press(inside, "Escape").stopped, true);
  assert.equal(rows(render()).filter((row) => row.props.multiSelected).length, 0, "owned Escape clears the selection");
  embedded = false; selectBoth(); document.activeElement = remote;
  assert.equal(press(remote, "F2").stopped, true, "standalone retains its existing window-wide multi-selection guard");
  h.close();
  assert.ok([...listeners.values()].every((set) => set.size === 0));
});


test("search and approval dialogs keep workspace navigation shortcuts from changing the page behind them", () => {
  const f = workspaceFixture(); f.render(); f.render();
  f.key(f.inside, "k"); f.render();
  assert.equal(f.key(f.inside, "n").defaultPrevented, false);
  assert.deepEqual(f.calls, []);
  assert.equal(f.key(f.inside, "b").defaultPrevented, false);
  assert.equal(f.key(f.inside, "k").defaultPrevented, true, "the search shortcut still closes its own panel");
  f.render();
  f.document.documentElement.dataset.modal = "1";
  assert.equal(f.key(f.inside, "n").defaultPrevented, false);
  assert.deepEqual(f.calls, []);
  delete f.document.documentElement.dataset.modal;
  assert.equal(f.key(f.inside, "n").defaultPrevented, true);
  assert.deepEqual(f.calls, [{ place: null }]);
  f.close();
});


test("transient panels restore a connected opener after unmount without stealing a new focus target", () => {
  const run = (nextFocus, connected = true) => {
    const h = hooks(), frames = [];
    const body = new Element(), opener = new Element(), panel = new Element(), field = new Element(panel), outside = new Element();
    opener.isConnected = true; field.isConnected = true; outside.isConnected = true;
    const document = { body, activeElement: opener };
    let restored = 0;
    opener.focus = () => { restored++; document.activeElement = opener; };
    const { useReturnFocus } = compiled("ui/workspace/parts.tsx", {
      react: h.react, "react/jsx-runtime": runtime, "../../lib/i18n": { t: (key) => key }, "../../lib/platform": { detectOs: () => "mac" },
      "../util": {}, "lucide-react": {},
    }, { document, HTMLElement: Element, requestAnimationFrame: (fn) => frames.push(fn), cancelAnimationFrame() {} });
    const ref = { current: panel };
    h.render(() => { useReturnFocus(ref); return null; }, {});
    document.activeElement = field; h.close();
    opener.isConnected = connected;
    document.activeElement = nextFocus === "body" ? body : outside;
    frames.splice(0).forEach((fn) => fn());
    return restored;
  };
  assert.equal(run("body"), 1);
  assert.equal(run("outside"), 0, "a click or navigation that already moved focus wins over the old opener");
  assert.equal(run("body", false), 0, "a removed opener is never focused");
});


test("pairing cannot navigate during connection, and a failed revocation stays visible and retryable", async () => {
  const h = hooks();
  h.react.useContext = () => null;
  let guard = null, finishPair, failRevocation = true, navigated = 0;
  const pairResult = new Promise((resolve) => { finishPair = resolve; });
  const Confirm = () => null;
  const { PairPage } = compiled("ui/pages/Pair.tsx", {
    react: h.react, "react/jsx-runtime": runtime, "../../lib/i18n": { t: (key) => key },
    "../../lib/api": { api: { pair: () => pairResult, unpair: async () => { if (failRevocation) throw new Error("revocation unavailable"); } } },
    "../ComputerTabs": { Confirm }, "../glyphs": { SeatMark: () => null }, "../Icon": { Icon: () => null },
    "../PairCode": { groupCode: (value) => value, looksLikeTicket: () => false, toDigits: (value) => value.replace(/\D/g, "") },
    "../Shell": { ShellContext: {} }, "../util": { cx: (...xs) => xs.filter(Boolean).join(" ") }, "./common": { levelName: () => "control" },
  });
  const props = { onPaired() {}, registerGuard: (value) => { guard = value; } };
  const render = () => h.render(PairPage, props);
  const button = (tree, label) => nodes(tree).find((node) => node.type === "button" && node.props.children === label);
  let tree = render();
  nodes(tree).find((node) => node.type === "input" && node.props.inputMode === "numeric").props.onChange({ target: { value: "12345678" } });
  tree = render(); button(tree, "pair_btn").props.onClick(); tree = render();
  assert.equal(guard(() => navigated++), true);
  assert.equal(navigated, 0);
  assert.equal(nodes(tree).find((node) => node.props?.inputMode === "numeric").props.disabled, true);
  finishPair({ peer: "fixture-host", sas: "one-two", level: "control" });
  await pairResult; await new Promise((resolve) => setImmediate(resolve)); tree = render();
  assert.ok(button(tree, "no_match"), JSON.stringify(nodes(tree).filter((node) => node.type === "button").map((node) => node.props.children)));
  await button(tree, "no_match").props.onClick(); tree = render();
  assert.ok(button(tree, "match"), "a failed mismatch revocation keeps the safety words on screen");
  assert.ok(nodes(tree).some((node) => node.props?.role === "alert" && /revocation unavailable/.test(node.props.children)));
  guard(() => navigated++); tree = render();
  let confirm = nodes(tree).find((node) => node.type === Confirm);
  await assert.rejects(confirm.props.onYes, /revocation unavailable/);
  assert.equal(navigated, 0);
  assert.ok(nodes(render()).some((node) => node.type === Confirm));
  failRevocation = false;
  confirm = nodes(render()).find((node) => node.type === Confirm);
  await confirm.props.onYes(); render();
  assert.equal(navigated, 1, "navigation resumes only after the tentative pairing is revoked");
  h.close();
});


test("a portaled sheet retains its opening pane ownership without consuming keys while inert", () => {
  const f = workspaceFixture(); f.render(); f.render();
  const modal = new Element(); modal.modalRoot = true;
  const button = new Element(modal);
  f.root.inert = true;
  f.document.activeElement = button;
  f.documentEvent("focusin", button); f.documentEvent("pointerdown", button);
  assert.equal(f.key(f.body, "n").defaultPrevented, false);
  assert.deepEqual(f.calls, []);
  f.root.inert = false;
  f.document.activeElement = f.body; f.documentEvent("focusin", f.body);
  assert.equal(f.key(f.body, "b").defaultPrevented, true, "the sheet may have removed its menu opener; body keeps the original pane");
  f.documentEvent("pointerdown", f.remote);
  assert.equal(f.key(f.body, "n").defaultPrevented, false, "real interaction with the screen still transfers ownership");
  f.close();
});
