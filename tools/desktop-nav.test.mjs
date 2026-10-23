// The desktop shell's navigation and keyboard shortcuts (apps/desktop/src/lib/nav.ts, shortcuts.ts):
// every deep link the screenshots, docs and e2e scripts open, the history (push / back / forward /
// replace / parentOf / canBack), tray intents, the sidebar's remembered state, and the shortcut table
// on both platforms with its guards. Both modules have only `import type`, so they run in a vm.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

function load(rel) {
  const source = readFileSync(new URL(`../apps/desktop/src/${rel}`, import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const exports = {};
  vm.runInNewContext(compiled, { exports, URLSearchParams, JSON, require: () => { throw new Error("no dependencies"); } }, { filename: rel });
  return exports;
}

const nav = load("lib/nav.ts");
const keys = load("lib/shortcuts.ts");
const plain = (v) => JSON.parse(JSON.stringify(v));
const mem = { sec: "network", tab: "tools" };
const route = (search, m = nav.DEFAULT_MEMORY) => plain(nav.parseRoute(search, m));
const hist = (...stack) => ({ stack, i: stack.length - 1 });

test("every deep link lands on its page", () => {
  assert.deepEqual(route("?view=devices"), { route: { page: "devices" }, explicit: true });
  assert.deepEqual(route("?view=approvals"), { route: { page: "approvals" }, explicit: true });
  assert.deepEqual(route("?view=pair"), { route: { page: "pair" }, explicit: true });
  assert.deepEqual(route("?view=activity"), { route: { page: "activity" }, explicit: true });
  assert.deepEqual(route("?view=activity&host=office-pc"), { route: { page: "activity", host: "office-pc" }, explicit: true });
  for (const sec of ["general", "agents", "device", "network", "approvals", "about"]) {
    assert.deepEqual(route(`?shotlang=zh&view=settings&sec=${sec}`), { route: { page: "settings", sec }, explicit: true }, sec);
  }
  assert.deepEqual(route("?view=computer&host=office-pc"), { route: { page: "computer", host: "office-pc", tab: "overview" }, explicit: true });
  for (const tab of ["overview", "access", "tools", "agents"]) {
    assert.deepEqual(route(`?view=computer&host=office-pc&tab=${tab}`).route, { page: "computer", host: "office-pc", tab }, tab);
  }
  // the e2e entry: must stay exactly this
  assert.deepEqual(route("?view=session&host=lab-pc&panel=chat"), { route: { page: "session", host: "lab-pc", panel: "chat" }, explicit: true });
  assert.deepEqual(route("?view=session&host=lab-pc"), { route: { page: "session", host: "lab-pc" }, explicit: true });
  // first run: welcome forces the unwelcomed state, empty the welcomed one on the (empty) devices page
  assert.deepEqual(route("?empty&view=welcome"), { route: { page: "welcome" }, explicit: false, welcomed: false });
  assert.deepEqual(route("?view=empty"), { route: { page: "devices" }, explicit: false, welcomed: true });
});

test("deep links fall back sensibly", () => {
  // no view, an unknown one, or a computer / session without a host: the devices page, not explicit
  for (const s of ["", "?shotlang=en", "?view=nonsense", "?view=computer", "?view=session&panel=chat"]) {
    assert.deepEqual(route(s), { route: { page: "devices" }, explicit: false }, s);
  }
  // a missing or bad section uses the remembered one; a bad tab is the overview
  assert.deepEqual(route("?view=settings", mem).route, { page: "settings", sec: "network" });
  assert.deepEqual(route("?view=settings&sec=bogus", mem).route, { page: "settings", sec: "network" });
  assert.deepEqual(route("?view=settings").route, { page: "settings", sec: "general" });
  assert.deepEqual(route("?view=computer&host=a&tab=activity").route, { page: "computer", host: "a", tab: "overview" });
  // the screenshot switches are not routes
  assert.deepEqual(route("?shotlang=zh&shottheme=dark&shotstate=x&locked&view=devices"), { route: { page: "devices" }, explicit: true });
});

test("push adds history, drops the forward part, and never doubles the same place", () => {
  let h = hist({ page: "devices" });
  h = nav.push(h, { page: "computer", host: "a", tab: "overview" });
  h = nav.push(h, { page: "session", host: "a" });
  assert.equal(h.i, 2);
  assert.equal(h.stack.length, 3);
  // the same place again (a session with a chat target is still the same session) replaces it
  const again = nav.push(h, { page: "session", host: "a", chat: { id: "x" } });
  assert.equal(again.stack.length, 3);
  assert.deepEqual(plain(again.stack[2]), { page: "session", host: "a", chat: { id: "x" } });
  // going back then somewhere new drops what was ahead
  const back = nav.back(h);
  assert.equal(back.i, 1);
  const branch = nav.push(back, { page: "settings", sec: "general" });
  assert.deepEqual(plain(branch.stack.map((r) => r.page)), ["devices", "computer", "settings"]);
  assert.equal(nav.canForward(branch), false);
});

test("history keeps at most 50 entries", () => {
  let h = hist({ page: "devices" });
  for (let n = 0; n < 80; n++) h = nav.push(h, { page: "computer", host: `pc-${n}`, tab: "overview" });
  assert.equal(h.stack.length, nav.HISTORY_LIMIT);
  assert.equal(h.i, nav.HISTORY_LIMIT - 1);
  assert.equal(h.stack[h.i].host, "pc-79");
});

test("back and forward walk the history; with none, back goes to the logical parent", () => {
  const h = hist({ page: "devices" }, { page: "approvals" }, { page: "pair" });
  const b1 = nav.back(h);
  assert.equal(nav.current(b1).page, "approvals");
  assert.equal(nav.canForward(b1), true);
  const f1 = nav.forward(b1);
  assert.equal(nav.current(f1).page, "pair");
  assert.equal(nav.forward(f1), f1, "no forward history: unchanged");
  // a deep link straight into a session: ‹ goes to its computer with the remembered tab
  const deep = hist({ page: "session", host: "lab-pc", panel: "chat" });
  assert.equal(nav.canBack(deep, mem), true);
  assert.deepEqual(plain(nav.current(nav.back(deep, mem))), { page: "computer", host: "lab-pc", tab: "tools" });
  // the devices page and the welcome have no parent: ‹ is disabled and does nothing
  for (const r of [{ page: "devices" }, { page: "welcome" }]) {
    const only = hist(r);
    assert.equal(nav.canBack(only), false, r.page);
    assert.equal(nav.back(only), only, r.page);
  }
});

test("parentOf: session → computer, computer → devices, a computer's activity → that computer", () => {
  assert.deepEqual(plain(nav.parentOf({ page: "session", host: "a" }, mem)), { page: "computer", host: "a", tab: "tools" });
  assert.deepEqual(plain(nav.parentOf({ page: "computer", host: "a", tab: "access" }, mem)), { page: "devices" });
  assert.deepEqual(plain(nav.parentOf({ page: "activity", host: "a" }, mem)), { page: "computer", host: "a", tab: "tools" });
  for (const r of [{ page: "activity" }, { page: "approvals" }, { page: "pair" }, { page: "settings", sec: "about" }]) {
    assert.deepEqual(plain(nav.parentOf(r, mem)), { page: "devices" }, r.page);
  }
  assert.equal(nav.parentOf({ page: "devices" }), null);
  assert.equal(nav.parentOf({ page: "welcome" }), null);
});

test("replace swaps the current entry: tabs, sections and filters make no history", () => {
  const h = hist({ page: "devices" }, { page: "computer", host: "a", tab: "overview" });
  const r = nav.replace(h, { page: "computer", host: "a", tab: "agents" });
  assert.equal(r.stack.length, 2);
  assert.equal(r.i, 1);
  assert.equal(r.stack[1].tab, "agents");
  assert.equal(h.stack[1].tab, "overview", "the old history object is untouched");
});

test("same() ignores the chat target but not the host, tab, section or filter", () => {
  assert.equal(nav.same({ page: "session", host: "a" }, { page: "session", host: "a", chat: { id: 1 } }), true);
  assert.equal(nav.same({ page: "session", host: "a" }, { page: "session", host: "a", panel: "chat" }), false);
  assert.equal(nav.same({ page: "session", host: "a" }, { page: "session", host: "b" }), false);
  assert.equal(nav.same({ page: "computer", host: "a", tab: "tools" }, { page: "computer", host: "a", tab: "access" }), false);
  assert.equal(nav.same({ page: "settings", sec: "about" }, { page: "settings", sec: "about" }), true);
  assert.equal(nav.same({ page: "activity" }, { page: "activity", host: "a" }), false);
  assert.equal(nav.same({ page: "devices" }, { page: "approvals" }), false);
});

test("tray intents: every menu item opens Settings › This device; no view stays put", () => {
  const payloads = [
    [{ view: "settings", sec: "device" }, { page: "settings", sec: "device" }],
    [{ view: "settings", sec: "device", action: "pair_code" }, { page: "settings", sec: "device" }],
    [{ view: "settings", sec: "device", anchor: "recent" }, { page: "settings", sec: "device" }],
    [{ view: "settings", sec: "device", action: "confirm_stop" }, { page: "settings", sec: "device" }],
    [{ view: "settings", sec: "device", action: "enable" }, { page: "settings", sec: "device" }],
    [{ view: "settings", sec: "device", error: "boom" }, { page: "settings", sec: "device" }],
    [{ view: "settings" }, { page: "settings", sec: "device" }],
    [{ view: "settings", sec: "network" }, { page: "settings", sec: "network" }],
    [{ view: "settings", sec: "nope" }, { page: "settings", sec: "device" }],
    [{ view: "approvals" }, { page: "approvals" }],
  ];
  for (const [p, want] of payloads) assert.deepEqual(plain(nav.fromTray(p)), want, JSON.stringify(p));
  assert.equal(nav.fromTray({}), null);
  assert.equal(nav.fromTray(null), null);
  assert.equal(nav.fromTray({ view: "session" }), null);
});

test("the window going to the tray turns a live session into its computer page", () => {
  const h = hist({ page: "devices" }, { page: "session", host: "a", panel: "chat" });
  const after = nav.hidden(h, mem);
  assert.equal(after.stack.length, 2);
  assert.deepEqual(plain(nav.current(after)), { page: "computer", host: "a", tab: "tools" });
  const other = hist({ page: "settings", sec: "device" });
  assert.equal(nav.hidden(other, mem), other);
});

test("the sidebar highlights a computer for its page and its session", () => {
  assert.deepEqual(plain(nav.navKey({ page: "session", host: "a" })), { computer: "a" });
  assert.deepEqual(plain(nav.navKey({ page: "computer", host: "a", tab: "tools" })), { computer: "a" });
  assert.equal(nav.navKey({ page: "activity", host: "a" }), "activity");
  assert.equal(nav.navKey({ page: "settings", sec: "about" }), "settings");
  assert.equal(nav.navKey({ page: "welcome" }), null);
  assert.deepEqual(plain(nav.routeOfNav("settings", mem)), { page: "settings", sec: "network" });
  assert.deepEqual(plain(nav.routeOfNav({ computer: "a" }, mem)), { page: "computer", host: "a", tab: "tools" });
  assert.deepEqual(plain(nav.routeOfNav("home", mem)), { page: "devices" });
  assert.deepEqual(plain(nav.routeOfNav("pair", mem)), { page: "pair" });
});

test("memory: the last section and tab are remembered, junk is ignored", () => {
  const m = nav.remember(nav.DEFAULT_MEMORY, { page: "settings", sec: "about" });
  assert.deepEqual(plain(m), { sec: "about", tab: "overview" });
  assert.equal(nav.remember(m, { page: "devices" }), m, "unchanged memory is the same object");
  assert.deepEqual(plain(nav.remember(m, { page: "computer", host: "a", tab: "agents" })), { sec: "about", tab: "agents" });
  assert.deepEqual(plain(nav.parseMemory('{"sec":"network","tab":"tools"}')), { sec: "network", tab: "tools" });
  assert.deepEqual(plain(nav.parseMemory('{"sec":"x","tab":1}')), { sec: "general", tab: "overview" });
  assert.deepEqual(plain(nav.parseMemory("not json")), { sec: "general", tab: "overview" });
  assert.deepEqual(plain(nav.parseMemory(null)), { sec: "general", tab: "overview" });
});

test("the sidebar: expanded on pages, compact on sessions and narrow windows, the person's choice wins", () => {
  assert.equal(nav.sidebarState({}, "shell", 1280), "expanded");
  assert.equal(nav.sidebarState({}, "shell", 1000), "compact");
  assert.equal(nav.sidebarState({}, "session", 1800), "compact");
  assert.equal(nav.sidebarState({ session: "expanded" }, "session", 1000), "expanded");
  assert.equal(nav.sidebarState({ shell: "compact" }, "shell", 1800), "compact");
  assert.equal(nav.sidebarState({ shell: "expanded" }, "shell", 1000), "expanded");
  assert.deepEqual(plain(nav.parseSidebarPref('{"shell":"compact","session":"hidden","x":1}')), { shell: "compact" });
  assert.deepEqual(plain(nav.parseSidebarPref("{")), {});
});

const ev = (key, mods = {}, extra = {}) => ({ key, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...mods, ...extra });
const kind = (a) => (a ? (a.kind === "page" ? `page:${a.page}` : a.kind) : null);

test("macOS shortcuts", () => {
  const cmd = { metaKey: true };
  const table = [
    [ev("[", cmd, { code: "BracketLeft" }), "back"],
    [ev("]", cmd, { code: "BracketRight" }), "forward"],
    [ev(",", cmd), "settings"],
    [ev("1", cmd), "page:devices"],
    [ev("2", cmd), "page:approvals"],
    [ev("3", cmd), "page:activity"],
    [ev("n", cmd), "pair"],
    [ev("f", cmd), "search"],
    [ev("\\", cmd), "sidebar"],
    [ev("s", { metaKey: true, ctrlKey: true }), "sidebar"],
    // not ours: the session's full screen, and plain or shifted keys
    [ev("f", { metaKey: true, ctrlKey: true }), null],
    [ev("[", {}), null],
    [ev("n", { metaKey: true, shiftKey: true }), null],
    [ev("4", cmd), null],
    [ev("ArrowLeft", { altKey: true }), null],
    [ev("F11"), null],
  ];
  for (const [e, want] of table) assert.equal(kind(keys.shortcutOf(e, "mac")), want, JSON.stringify(e));
});

test("Windows shortcuts", () => {
  const ctrl = { ctrlKey: true };
  const table = [
    [ev("ArrowLeft", { altKey: true }), "back"],
    [ev("ArrowRight", { altKey: true }), "forward"],
    [ev(",", ctrl), "settings"],
    [ev("1", ctrl), "page:devices"],
    [ev("2", ctrl), "page:approvals"],
    [ev("3", ctrl), "page:activity"],
    [ev("n", ctrl), "pair"],
    [ev("f", ctrl), "search"],
    [ev("\\", ctrl), "sidebar"],
    [ev("[", { metaKey: true }), null],
    [ev("ArrowLeft", { altKey: true, ctrlKey: true }), null],
    [ev("f", { ctrlKey: true, altKey: true }), null],
    [ev("F11"), null],
  ];
  for (const [e, want] of table) assert.equal(kind(keys.shortcutOf(e, "windows")), want, JSON.stringify(e));
  // F11 is full screen only on a live session
  assert.equal(kind(keys.shortcutOf(ev("F11"), "windows", { session: true })), "fullscreen");
});

test("shortcut guards: input methods, the remote picture, dialogs, the chat's own find", () => {
  const cmdF = ev("f", { metaKey: true });
  assert.equal(keys.shortcutOf({ ...cmdF, isComposing: true }, "mac"), null);
  assert.equal(keys.shortcutOf({ ...ev("1", { ctrlKey: true }), keyCode: 229 }, "windows"), null);
  assert.equal(keys.shortcutOf({ ...cmdF, defaultPrevented: true }, "mac"), null);
  assert.equal(keys.shortcutOf(ev("[", { metaKey: true }), "mac", { inPicture: true }), null, "keys belong to the remote computer");
  assert.equal(keys.shortcutOf(ev(",", { ctrlKey: true }), "windows", { modal: true }), null, "a dialog is open");
  assert.equal(keys.shortcutOf(cmdF, "mac", { inChat: true }), null, "the chat finds in the conversation");
  assert.equal(kind(keys.shortcutOf(ev("1", { metaKey: true }), "mac", { inChat: true })), "page:devices", "other keys still work in the chat");
  assert.equal(keys.shortcutOf(ev("F11"), "windows", { session: true, inPicture: true }), null);
});

test("shortcuts read the character, the physical key only for non-Latin layouts", () => {
  // a Dvorak "u" sits where QWERTY has F: not a search
  assert.equal(keys.shortcutOf(ev("u", { metaKey: true }, { code: "KeyF" }), "mac"), null);
  // a Russian layout types "а" on that key: the physical key decides
  assert.equal(kind(keys.shortcutOf(ev("а", { metaKey: true }, { code: "KeyF" }), "mac")), "search");
  assert.equal(kind(keys.shortcutOf(ev("&", { ctrlKey: true }, { code: "Digit1" }), "windows")), null);
  assert.equal(kind(keys.shortcutOf(ev("ё", { ctrlKey: true }, { code: "Digit2" }), "windows")), "page:approvals");
});

test("mouse buttons 3 and 4 go back and forward, never over the remote picture", () => {
  assert.equal(kind(keys.mouseActionOf(3)), "back");
  assert.equal(kind(keys.mouseActionOf(4)), "forward");
  assert.equal(keys.mouseActionOf(0), null);
  assert.equal(keys.mouseActionOf(3, { inPicture: true }), null);
  assert.equal(keys.shortcutLabel("back", "mac"), "⌘[");
  assert.equal(keys.shortcutLabel("sidebar", "windows"), "Ctrl+\\");
});
