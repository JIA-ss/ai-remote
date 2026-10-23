// The agents workspace's data (apps/desktop/src/lib/workspace.ts): grouping, sorting, AND search,
// unread marks, the status dots and their priority, fallback titles, and what is done for a host that
// has no `chat_update`. The module is transpiled as it is; its one import (lib/chat.ts, pure) is
// transpiled the same way.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

function makeStorage() {
  const store = new Map();
  return { getItem: (k) => store.get(k) ?? null, setItem: (k, v) => store.set(k, String(v)), removeItem: (k) => store.delete(k), store };
}

/** Transpiles `lib/<name>.ts` and runs it; `require("./x")` loads `lib/x.ts` the same way. */
function load(name, localStorage = makeStorage(), cache = new Map()) {
  if (cache.has(name)) return cache.get(name);
  const source = readFileSync(new URL(`../apps/desktop/src/lib/${name}.ts`, import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const exports = {};
  cache.set(name, exports);
  const require = (spec) => {
    if (!spec.startsWith("./")) throw new Error(`no dependency ${spec}`);
    return load(spec.slice(2), localStorage, cache);
  };
  vm.runInNewContext(compiled, { exports, require, localStorage, JSON, Set, Map, Intl, Promise, setTimeout, clearTimeout, Date }, { filename: `${name}.ts` });
  return exports;
}

const ws = load("workspace");
const plain = (v) => JSON.parse(JSON.stringify(v));
let n = 0;
/** A session as a new host reports it. */
const meta = (over = {}) => ({
  id: `s${++n}`,
  runtime: "claude",
  name: "Claude Code",
  cwd: "/home/me/app",
  state: "idle",
  started_at: 1000,
  turns: 1,
  last_activity: 2000,
  seq: 10,
  last_seq: 10,
  ...over,
});
const item = (host, m, local = false) => ({ host, key: `${host}/${m.id}`, meta: m, local, online: true });

test("sessions group by folder and computer; pinned ones go to their own section", () => {
  const a = item("mac", meta({ cwd: "/home/me/app", last_activity: 50 }), true);
  const b = item("mac", meta({ cwd: "/home/me/app/", last_activity: 90 }), true);
  const c = item("win", meta({ cwd: "D:\\Work\\App", last_activity: 70 }));
  const d = item("win", meta({ cwd: "d:/work/app", last_activity: 80 }));
  const e = item("win", meta({ cwd: "D:\\work\\zeta", last_activity: 99 }));
  const pin = item("win", meta({ cwd: "D:\\work\\zeta", pinned: true, last_activity: 10 }));
  const { pinned, groups } = ws.groupSessions([a, b, c, d, e, pin], { pinned: (x) => !!x.meta.pinned, title: (x) => x.meta.id });
  assert.deepEqual(plain(pinned.map((x) => x.key)), [pin.key]);
  // Two folders named "app" (this computer's and win's), so each label says where it is, as the
  // group's latest session spells it.
  assert.deepEqual(plain(groups.map((g) => g.label)), ["app · /home/me", "app · d:/work · win", "zeta · win"]);
  // Inside a group: most recent first; a trailing slash and a Windows path's case or slashes are the same folder.
  assert.deepEqual(plain(groups[0].items.map((x) => x.key)), [b.key, a.key]);
  assert.deepEqual(plain(groups[1].items.map((x) => x.key)), [d.key, c.key]);
  assert.equal(groups[0].local, true);
});

test("a twin folder under the host user's home is shown with ~", () => {
  const a = item("mac", meta({ cwd: "/Users/me/code/site" }), true);
  const b = item("mac", meta({ cwd: "/Users/me/old/site" }), true);
  const { groups } = ws.groupSessions([a, b], { pinned: () => false, title: (x) => x.meta.id, home: () => "/Users/me" });
  assert.deepEqual(plain(groups.map((g) => g.label)), ["site · ~/code", "site · ~/old"]);
});

test("group labels sort as people read numbers, this computer first on a tie", () => {
  const xs = [
    item("b", meta({ cwd: "/p/v10" })),
    item("a", meta({ cwd: "/p/v9" })),
    item("z", meta({ cwd: "/q/w" }), true),
    item("y", meta({ cwd: "/q/w" })),
  ];
  const { groups } = ws.groupSessions(xs, { pinned: () => false, title: (x) => x.meta.id });
  assert.deepEqual(plain(groups.map((g) => g.label)), ["v9 · a", "v10 · b", "w · /q", "w · /q · y"]);
});

test("sessions sort by activity, by start or by title, and reverse", () => {
  const xs = [
    item("h", meta({ id: "a", title: "item 10", started_at: 3, last_activity: 1 })),
    item("h", meta({ id: "b", title: "Item 9", started_at: 1, last_activity: 3 })),
    item("h", meta({ id: "c", title: "alpha", started_at: 2, last_activity: 2 })),
  ];
  const title = (x) => x.meta.title;
  const ids = (list) => plain(list.map((x) => x.meta.id));
  assert.deepEqual(ids(ws.sortSessions(xs, "recency", title)), ["b", "c", "a"]);
  assert.deepEqual(ids(ws.sortSessions(xs, "created", title)), ["a", "c", "b"]);
  assert.deepEqual(ids(ws.sortSessions(xs, "alpha", title)), ["c", "b", "a"]);
  assert.deepEqual(ids(ws.sortSessions(xs, "recency", title, true)), ["a", "c", "b"]);
});

test("a group shows five rows, keeps the open one, and says when the rest needs you", () => {
  const rows = Array.from({ length: 8 }, (_, i) => ({ key: `k${i}`, waits: i === 6 }));
  const cut = ws.truncateGroup(rows, 5, new Set(["k7"]), false, (x) => x.waits);
  assert.deepEqual(plain(cut.shown.map((x) => x.key)), ["k0", "k1", "k2", "k3", "k4", "k7"]);
  assert.deepEqual(plain(cut.hidden.map((x) => x.key)), ["k5", "k6"]);
  assert.equal(cut.hiddenNeedsYou, true);
  assert.equal(ws.truncateGroup(rows, 5, new Set(), true, () => false).shown.length, 8);
  assert.equal(ws.truncateGroup(rows.slice(0, 5), 5, new Set(), false, () => true).hiddenNeedsYou, false);
});

test("search words are split at non-letters, lower case; symbols alone are one word", () => {
  assert.deepEqual(plain(ws.searchTokens("  Fix-the BUILD ")), ["fix", "the", "build"]);
  assert.deepEqual(plain(ws.searchTokens("修复 构建")), ["修复", "构建"]);
  assert.deepEqual(plain(ws.searchTokens("#!")), ["#!"]);
  assert.deepEqual(plain(ws.searchTokens("   ")), []);
});

test("search needs every word (AND): title matches first, then title / folder / branch / computer", () => {
  const xs = [
    item("win", meta({ id: "t1", title: "Fix export bug", cwd: "D:\\work\\report", branch: "main", last_activity: 10 })),
    item("mac", meta({ id: "t2", title: "Fix flaky export", cwd: "/x/site", branch: "ci", last_activity: 30 })),
    item("win", meta({ id: "t3", title: "Tidy imports", cwd: "D:\\work\\report", branch: "export-fix", last_activity: 50 })),
    item("win", meta({ id: "t4", title: "Fix login", cwd: "/x/site", branch: "main", last_activity: 40 })),
  ];
  const fields = (x) => ({ title: x.meta.title, folder: x.meta.cwd, branch: x.meta.branch ?? "", computer: x.host });
  const ids = (q) => plain(ws.searchSessions(xs, q, fields).map((x) => x.meta.id));
  assert.deepEqual(ids("export fix"), ["t2", "t1", "t3"]);
  assert.deepEqual(ids("fix site"), ["t4", "t2"]);
  assert.deepEqual(ids("report win tidy"), ["t3"]);
  assert.deepEqual(ids("export nothing"), []);
  // No query: every session, newest first.
  assert.deepEqual(ids(""), ["t3", "t4", "t2", "t1"]);
});

test("a session is unread when its last event is past the one seen here, unless it is open", () => {
  const m = meta({ last_seq: 12 });
  assert.equal(ws.isUnread(m, 10, false, false), true);
  assert.equal(ws.isUnread(m, 12, false, false), false);
  assert.equal(ws.isUnread(m, 10, false, true), false);
  assert.equal(ws.isUnread(m, 12, true, false), true);
  assert.equal(ws.isUnread({ ...m, archived: true }, 0, true, false), false);
  // An older host: `seq` stands for `last_seq`.
  const old = meta({ seq: 7 });
  delete old.last_seq;
  assert.equal(ws.lastSeq(old), 7);
  assert.equal(ws.isUnread(old, 5, false, false), true);
});

test("status dots: waiting for you, then running, then failed, then unread", () => {
  assert.equal(ws.dotOf(meta({ pending: "permission", state: "running" }), true), "awaiting");
  assert.equal(ws.dotOf(meta({ state: "running", last_status: "failed" }), true), "running");
  assert.equal(ws.dotOf(meta({ last_status: "failed" }), true), "failed");
  assert.equal(ws.dotOf(meta({ last_status: "completed" }), true), "unread");
  assert.equal(ws.dotOf(meta({ last_status: "completed" }), false), null);
  assert.equal(ws.dotOf(meta({ state: "starting" }), false), "running");
  assert.equal(ws.dotOf(meta({ pending: "question", state: "ended" }), false), null);
  assert.equal(ws.dotOf(meta({ archived: true, pending: "plan" }), true), null);
});

test("a folded group shows its most urgent dot: needs you > failed > unread > running", () => {
  assert.equal(ws.aggregateDot(["running", "unread", "failed", null]), "failed");
  assert.equal(ws.aggregateDot(["running", "awaiting", "failed"]), "awaiting");
  assert.equal(ws.aggregateDot(["running", "unread"]), "unread");
  assert.equal(ws.aggregateDot([null, null]), null);
});

test("titles fall back to the first message, then to what this device sent, then to the agent", () => {
  assert.equal(ws.displayTitle(meta({ title: "  Fix the build " })), "Fix the build");
  assert.equal(ws.displayTitle(meta({ first_text: "\n\n  make the export\nfaster please" })), "make the export");
  const long = "x".repeat(120);
  assert.equal(ws.displayTitle(meta({ first_text: long })).length, 80);
  assert.ok(ws.displayTitle(meta({ first_text: long })).endsWith("…"));
  assert.equal(ws.displayTitle(meta({}), "sent from here"), "sent from here");
  assert.equal(ws.displayTitle(meta({ title: "" })), "Claude Code");
  assert.equal(ws.displayTitle(meta({ name: "" })), "");
});

test("whether a host has chat_update shows in its sessions' fields", () => {
  assert.equal(ws.updatesFrom([]), null);
  assert.equal(ws.updatesFrom([{ id: "a", seq: 3 }]), false);
  assert.equal(ws.updatesFrom([{ id: "a", seq: 3 }, { id: "b", last_seq: 4 }]), true);
  assert.equal(ws.failureStatus("unknown tool 'chat_update'"), "unsupported");
  assert.equal(ws.failureStatus("'chat_sessions' requires level 'admin', but this device has 'control'."), "denied");
  assert.equal(ws.failureStatus("timed out"), "error");
});

test("the sidebar's width stays within 200–420 and the window's room; narrow windows fold it", () => {
  assert.equal(ws.clampSidebar(150, 1600), 200);
  assert.equal(ws.clampSidebar(500, 1600), 420);
  assert.equal(ws.clampSidebar(400, 650), 320);
  assert.equal(ws.narrowAt(288), 700);
  assert.equal(ws.narrowAt(420), 749);
});

test("recent places: most recently used first, at most three, the selected one kept", () => {
  const xs = [
    item("a", meta({ cwd: "/p/one", last_activity: 10 })),
    item("a", meta({ cwd: "/p/one/", last_activity: 40 })),
    item("b", meta({ cwd: "D:\\two", last_activity: 30 })),
    item("b", meta({ cwd: "D:\\three", last_activity: 20 })),
  ];
  const started = [{ host: "c", cwd: "/four", at: 35 }];
  const where = (list) => plain(list.map(({ host, cwd }) => ({ host, cwd })));
  assert.deepEqual(where(ws.recentPlaces(xs, started, null)), [{ host: "a", cwd: "/p/one" }, { host: "c", cwd: "/four" }, { host: "b", cwd: "D:\\two" }]);
  assert.deepEqual(where(ws.recentPlaces(xs, started, { host: "b", cwd: "d:\\THREE" })), [{ host: "a", cwd: "/p/one" }, { host: "c", cwd: "/four" }, { host: "b", cwd: "D:\\three" }]);
});

test("recent places keep the order shown; a new one goes before the first older one", () => {
  const p = (host, at) => ({ host, cwd: "/w", at });
  const shown = [p("a", 30), p("b", 20), p("c", 10)];
  // b became the most recent and d appeared between a and c: nothing shown moves, d goes before c.
  const next = [p("b", 50), p("a", 30), p("d", 15)];
  assert.deepEqual(plain(ws.keepOrder(shown, next).map((x) => x.host)), ["a", "b", "d"]);
  assert.deepEqual(plain(ws.keepOrder([], next).map((x) => x.host)), ["b", "a", "d"]);
});

test("next steps share three places: one per group, the rest to the first that has more", () => {
  const seat = (groups) => plain(ws.seatSteps(groups, 3));
  assert.deepEqual(seat([["f"], ["a", "b", "c", "d"]]), [{ shown: ["f"], rest: [] }, { shown: ["a"], rest: ["b", "c", "d"] }]);
  assert.deepEqual(seat([[], ["a", "b"]]), [{ shown: ["a", "b"], rest: [] }]);
  assert.deepEqual(seat([["a", "b", "c", "d", "e"]]), [{ shown: ["a", "b"], rest: ["c", "d", "e"] }]);
  assert.deepEqual(seat([["x"], ["y"], ["z"], ["w"]]), [{ shown: ["x"], rest: [] }, { shown: ["y"], rest: [] }, { shown: ["z"], rest: [] }]);
});

// ---- the store ----

function storeWith(answers, opts = {}) {
  const calls = [];
  const storage = opts.storage ?? makeStorage();
  const mod = load("workspace", storage);
  const call = async (host, tool, args) => {
    calls.push([host, tool, args]);
    const a = answers[`${host} ${tool}`];
    if (a instanceof Error) throw a;
    return typeof a === "function" ? a(args) : a;
  };
  const store = new mod.WorkspaceStore({ call, storage, now: () => 1 });
  store.setComputers(opts.computers ?? [{ name: "new", online: true, local: false }, { name: "old", online: true, local: false }]);
  return { store, calls, storage };
}

test("the store merges every online computer's sessions and counts what was there as seen", async () => {
  const { store } = storeWith({
    "new chat_sessions": { sessions: [meta({ id: "n1", last_activity: 5, last_seq: 4 }), meta({ id: "gone", archived: true })] },
    "old chat_sessions": { sessions: [{ id: "o1", runtime: "codex", name: "Codex", cwd: "/w", state: "idle", started_at: 1, turns: 1, last_activity: 9, seq: 3 }] },
  });
  await store.refreshAll();
  const s = store.get();
  assert.deepEqual(plain(store.items().map((x) => x.key)), ["old/o1", "new/n1"]);
  assert.equal(s.hosts.new.update, true);
  assert.equal(s.hosts.old.update, false);
  // Seen up to now: nothing is unread at first.
  assert.equal(store.items().some((x) => store.unread(x)), false);
  // The old host's session is titled by its agent until this device remembers what it sent.
  assert.equal(store.title(store.find("old", "o1")), "Codex");
  store.rememberFirst("old", "o1", "rename the columns\nand more");
  assert.equal(store.title(store.find("old", "o1")), "rename the columns");
});

test("new activity after the first look is unread until the session is opened", async () => {
  let seq = 4;
  const { store } = storeWith({ "new chat_sessions": () => ({ sessions: [meta({ id: "n1", last_seq: seq })] }), "old chat_sessions": { sessions: [] } });
  await store.refreshAll();
  seq = 9;
  await store.refresh("new");
  const x = store.find("new", "n1");
  assert.equal(store.unread(x), true);
  assert.equal(store.dot(x), "unread");
  store.setCurrent(x.key);
  assert.equal(store.unread(store.find("new", "n1")), false);
  // Marked unread by hand: stays so until opened again or marked read.
  store.setCurrent(null);
  store.markUnread(x.key);
  assert.equal(store.unread(store.find("new", "n1")), true);
  store.markRead(x.key);
  assert.equal(store.unread(store.find("new", "n1")), false);
});

test("a session first seen after the first look is unread when it has events", async () => {
  let list = [meta({ id: "n1" })];
  const { store } = storeWith({ "new chat_sessions": () => ({ sessions: list }), "old chat_sessions": { sessions: [] } });
  await store.refreshAll();
  list = [...list, meta({ id: "n2", last_seq: 3 })];
  await store.refresh("new");
  assert.equal(store.unread(store.find("new", "n2")), true);
});

test("pins, titles and archive go to chat_update; a host without it keeps pins here and refuses the rest", async () => {
  const { store, calls } = storeWith({
    "new chat_sessions": { sessions: [meta({ id: "n1" })] },
    "new chat_update": (args) => ({ ...meta({ id: "n1" }), pinned: !!args.pinned, title: args.title ?? null }),
    "old chat_sessions": { sessions: [{ id: "o1", runtime: "codex", name: "Codex", cwd: "/w", state: "idle", started_at: 1, turns: 1, last_activity: 9, seq: 3 }] },
  });
  await store.refreshAll();
  await store.update("new", "n1", { pinned: true });
  assert.deepEqual(plain(calls.at(-1)), ["new", "chat_update", { session: "n1", pinned: true }]);
  assert.equal(store.isPinned(store.find("new", "n1")), true);
  // The old host: no call at all, the pin is this device's.
  const before = calls.length;
  await store.update("old", "o1", { pinned: true });
  assert.equal(calls.length, before);
  assert.equal(store.isPinned(store.find("old", "o1")), true);
  await assert.rejects(store.update("old", "o1", { title: "x" }), /unsupported/);
  await assert.rejects(store.update("old", "o1", { archived: true }), /unsupported/);
  await store.update("old", "o1", { pinned: false });
  assert.equal(store.isPinned(store.find("old", "o1")), false);
});

test("a host that turns out not to have chat_update is learned from its refusal", async () => {
  const { store } = storeWith(
    {
      "new chat_sessions": { sessions: [] },
      "new chat_update": new Error("unknown tool 'chat_update'"),
    },
    { computers: [{ name: "new", online: true, local: false }] },
  );
  await store.refreshAll();
  store.addSession("new", meta({ id: "n9", title: "Started here" }));
  await store.update("new", "n9", { pinned: true });
  assert.equal(store.get().hosts.new.update, false);
  assert.equal(store.isPinned(store.find("new", "n9")), true);
  await assert.rejects(store.update("new", "n9", { title: "x" }), /unsupported/);
  // What was shown is put back.
  assert.equal(store.title(store.find("new", "n9")), "Started here");
});

test("archiving hides a session; undoing brings it back; a refused rename is put back", async () => {
  const { store } = storeWith(
    {
      "new chat_sessions": { sessions: [meta({ id: "n1", title: "Keep me" })] },
      "new chat_update": (args) => {
        if (args.title === "boom") throw new Error("session is busy");
        return { ...meta({ id: "n1", title: "Keep me" }), archived: !!args.archived };
      },
    },
    { computers: [{ name: "new", online: true, local: false }] },
  );
  await store.refreshAll();
  const known = store.find("new", "n1").meta;
  await store.update("new", "n1", { archived: true });
  assert.equal(store.find("new", "n1"), null);
  await store.update("new", "n1", { archived: false }, known);
  assert.equal(store.title(store.find("new", "n1")), "Keep me");
  await assert.rejects(store.update("new", "n1", { title: "boom" }), /busy/);
  assert.equal(store.title(store.find("new", "n1")), "Keep me");
});

test("refusals are told apart: an old host, a level too low, another error", async () => {
  const { store } = storeWith(
    {
      "a chat_sessions": new Error("unknown tool 'chat_sessions'"),
      "b chat_sessions": new Error("'chat_sessions' requires level 'admin', but this device has 'observe'."),
      "c chat_sessions": new Error("connection lost"),
    },
    { computers: ["a", "b", "c", "d"].map((name) => ({ name, online: name !== "d", local: false })) },
  );
  await store.refreshAll();
  const h = store.get().hosts;
  assert.equal(h.a.status, "unsupported");
  assert.equal(h.b.status, "denied");
  assert.equal(h.c.status, "error");
  assert.equal(h.d, undefined);
});

test("sessions seen last time are shown before the computers answer", async () => {
  const storage = makeStorage();
  const first = storeWith({ "new chat_sessions": { sessions: [meta({ id: "n1", title: "From before" })] } }, { storage, computers: [{ name: "new", online: true, local: false }] });
  await first.store.refreshAll();
  const again = storeWith({}, { storage, computers: [{ name: "new", online: false, local: false }] });
  const x = again.store.find("new", "n1");
  assert.equal(again.store.get().hosts.new.status, "cached");
  assert.equal(again.store.title(x), "From before");
  assert.equal(x.online, false);
});

test("chat events seen on an open conversation update its title, state, what waits and how it ended", () => {
  assert.deepEqual(plain(ws.eventPatch({ t: "title", seq: 4, ts: 99, title: " Fix the build ", source: "agent" })), { last_seq: 4, last_activity: 99, title: "Fix the build", title_source: "agent" });
  assert.equal(ws.eventPatch({ t: "permission", seq: 5 }).pending, "permission");
  assert.equal(ws.eventPatch({ t: "plan_review", seq: 6 }).pending, "plan");
  assert.equal(ws.eventPatch({ t: "resolved", seq: 7 }).pending, null);
  assert.deepEqual(plain(ws.eventPatch({ t: "turn_end", seq: 8, status: "failed", reason: "exit 1" })), { last_seq: 8, pending: null, last_status: "failed", last_reason: "exit 1" });
  assert.equal(ws.eventPatch({ t: "state", seq: 9, state: "running" }).state, "running");
  // A line outside the conversation (seq -1) changes nothing.
  assert.equal(ws.eventPatch({ t: "commands", seq: -1 }), null);
});

test("leaving a session counts what it said meanwhile as seen", async () => {
  const { store } = storeWith({ "new chat_sessions": { sessions: [meta({ id: "n1", last_seq: 4 })] }, "old chat_sessions": { sessions: [] } });
  await store.refreshAll();
  store.setCurrent("new/n1");
  store.patchSession("new", "n1", ws.eventPatch({ t: "message", seq: 12, ts: 5 }));
  store.setCurrent(null);
  assert.equal(store.unread(store.find("new", "n1")), false);
});

// ---- The list's grouping, sorting and status filter (SB-09..SB-14), "Mark as done" (SB-26) ----

test("list settings: unknown values take the defaults; ascending is kept as 'turned around'", () => {
  assert.deepEqual(plain(ws.normalizeList({ groupBy: "folder", sort: "alpha", reversed: "yes", status: "archived" })), {
    groupBy: "project",
    sort: "alpha",
    reversed: false,
    status: "archived",
  });
  assert.equal(ws.isDefaultList(ws.normalizeList(null)), true);
  assert.equal(ws.isFiltered({ ...ws.DEFAULT_LIST, status: "all" }), true);
  assert.equal(ws.isFiltered({ ...ws.DEFAULT_LIST, groupBy: "date" }), false);
  // Names go A→Z unless turned around; times go newest first unless turned around.
  assert.equal(ws.isAscending({ ...ws.DEFAULT_LIST, sort: "alpha" }), true);
  assert.equal(ws.isAscending(ws.DEFAULT_LIST), false);
  assert.equal(ws.withAscending({ ...ws.DEFAULT_LIST, sort: "alpha" }, false).reversed, true);
  assert.equal(ws.withAscending(ws.DEFAULT_LIST, true).reversed, true);
  assert.equal(ws.rowsBeforeMore("none"), 20);
  assert.equal(ws.rowsBeforeMore("date"), 5);
});

test("the status filter lists sessions not archived, archived ones, or both", () => {
  const live = meta();
  const gone = meta({ archived: true });
  assert.deepEqual([live, gone].map((m) => ws.shownByStatus(m, "active")), [true, false]);
  assert.deepEqual([live, gone].map((m) => ws.shownByStatus(m, "archived")), [false, true]);
  assert.deepEqual([live, gone].map((m) => ws.shownByStatus(m, "all")), [true, true]);
});

test("by day: today, yesterday, one bucket per day for five days, then older; empty ones left out", () => {
  const now = new Date(2026, 9, 23, 15, 0).getTime();
  const at = (y, m, d, h = 12) => new Date(y, m, d, h).getTime();
  const xs = [
    meta({ id: "a", last_activity: at(2026, 9, 23, 1) }),
    meta({ id: "b", last_activity: at(2026, 9, 22, 23) }),
    meta({ id: "c", last_activity: at(2026, 9, 21) }),
    meta({ id: "d", last_activity: at(2026, 9, 17, 0) }),
    meta({ id: "e", last_activity: at(2026, 9, 16, 23) }),
    meta({ id: "f", last_activity: at(2025, 0, 1), started_at: at(2026, 9, 23, 2) }),
  ].map((m) => ({ meta: m }));
  const b = ws.dateBuckets(xs, now, "last_activity");
  assert.deepEqual(plain(b.map((x) => [x.key, x.items.map((i) => i.meta.id)])), [
    ["today", ["a"]],
    ["yesterday", ["b"]],
    ["day2", ["c"]],
    ["day6", ["d"]],
    ["older", ["e", "f"]],
  ]);
  assert.equal(b[2].day, at(2026, 9, 21, 0));
  // Sorting by start buckets by start; going up turns the buckets around.
  const byStart = ws.dateBuckets(xs, now, "started_at", true);
  assert.equal(byStart.at(-1).key, "today");
  assert.deepEqual(plain(byStart.at(-1).items.map((i) => i.meta.id)), ["f"]);
});

test("by state: needs you (and failed), working, to look at, done, idle — in that order", () => {
  const cases = [
    [meta({ pending: "permission" }), "awaiting", "blocked"],
    [meta({ last_status: "failed" }), "failed", "blocked"],
    [meta({ state: "running" }), "running", "working"],
    [meta({}), "unread", "ready"],
    [meta({ state: "ended" }), null, "done"],
    [meta({ archived: true }), null, "done"],
    [meta({}), null, "idle"],
  ];
  for (const [m, dot, want] of cases) assert.equal(ws.stateBucket(m, dot), want);
  const items = cases.map(([m, dot]) => ({ m, b: ws.stateBucket(m, dot) })).reverse();
  assert.deepEqual(plain(ws.stateBuckets(items, (x) => x.b).map((g) => [g.key, g.items.length])), [
    ["blocked", 2],
    ["working", 1],
    ["ready", 1],
    ["done", 2],
    ["idle", 1],
  ]);
});

test("'Mark as done' puts a failure aside until something new happens, and can be undone", async () => {
  let seq = 7;
  const { store, storage } = storeWith(
    { "new chat_sessions": () => ({ sessions: [meta({ id: "n1", last_status: "failed", last_seq: seq })] }) },
    { computers: [{ name: "new", online: true, local: false }] },
  );
  await store.refreshAll();
  const x = () => store.find("new", "n1");
  assert.equal(store.dot(x()), "failed");
  const undo = store.markDone("new/n1");
  assert.equal(store.dot(x()), null);
  assert.equal(JSON.parse(storage.store.get("ai-remote.ws.acked"))["new/n1"], 7);
  undo();
  assert.equal(store.dot(x()), "failed");
  store.markDone("new/n1");
  // A later turn that fails again shows again.
  seq = 9;
  await store.refreshAll();
  assert.equal(store.dot(x()), "failed");
  assert.equal(ws.isAcked(x().meta, 7), false);
});

test("archived sessions are asked for and kept only while the list shows them", async () => {
  const { store, calls } = storeWith(
    {
      "new chat_sessions": (args) => ({ sessions: [meta({ id: "n1" }), ...(args.archived ? [meta({ id: "n2", archived: true })] : [])] }),
      "new chat_update": (args) => ({ ...meta({ id: args.session }), archived: !!args.archived }),
    },
    { computers: [{ name: "new", online: true, local: false }] },
  );
  await store.refreshAll();
  assert.deepEqual(plain(store.items().map((x) => x.meta.id)), ["n1"]);
  store.setArchived(true);
  await store.refreshAll();
  assert.deepEqual(plain(calls.at(-1)), ["new", "chat_sessions", { archived: true }]);
  assert.deepEqual(plain(store.items().map((x) => x.meta.id).sort()), ["n1", "n2"]);
  // Archiving keeps the session listed (as archived); unarchiving brings it back.
  await store.update("new", "n1", { archived: true });
  assert.equal(store.find("new", "n1").meta.archived, true);
  await store.update("new", "n2", { archived: false });
  assert.equal(store.find("new", "n2").meta.archived, false);
  store.setArchived(false);
  assert.deepEqual(plain(store.items().map((x) => x.meta.id)), ["n2"]);
});
