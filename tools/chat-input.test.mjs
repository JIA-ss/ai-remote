// The chat composer's input side (apps/desktop/src/lib/chat.ts and chat-input.ts): messages on
// their way, the / and @ menus, attachments, find and quoting, with the real modules transpiled.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

function load(file, deps = {}) {
  const source = readFileSync(new URL(`../apps/desktop/src/lib/${file}`, import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const exports = {};
  const store = new Map();
  const localStorage = { getItem: (k) => store.get(k) ?? null, setItem: (k, v) => store.set(k, String(v)), removeItem: (k) => store.delete(k) };
  const require = (m) => {
    if (m in deps) return deps[m];
    throw new Error(`no dependency ${m}`);
  };
  vm.runInNewContext(compiled, { exports, require, localStorage, JSON, Set, Map, Promise, setTimeout, clearTimeout, Error, Math, Date, Number, String, RegExp, Array, Object }, { filename: file });
  return exports;
}

const chat = load("chat.ts");
const plain = (v) => JSON.parse(JSON.stringify(v));
let seq = 0;
const ev = (turn, e) => ({ seq: ++seq, ts: 1000 + seq * 100, turn, ...e });
const fold = (events) => events.reduce(chat.reduceChat, chat.emptyChat());

// ---------------------------------------------------------------------------------------------
// Messages on their way

const out = (id, state = "sending", at = 0) => ({ id, text: id, at, state });

test("a message leaves the list once the conversation shows it, as a user event or queued", () => {
  seq = 0;
  const c = fold([
    ev(0, { t: "user", input_id: "a", by: { device: "d" }, text: "a" }),
    ev(1, { t: "turn_start" }),
    ev(1, { t: "state", state: "running", queued: [{ id: "b", text: "b" }] }),
  ]);
  const left = chat.settleOutgoing([out("a", "acked"), out("b", "acked"), out("c", "acked")], c, 10, true);
  assert.deepEqual(plain(left).map((o) => o.id), ["c"]);
});

test("acks, refusals, timeouts and a dropped connection", () => {
  let list = [out("a"), out("b"), out("c")];
  list = chat.outgoingReply(list, { t: "ack", input_id: "a" });
  list = chat.outgoingReply(list, { t: "error", code: "refused", input_id: "b", text: "empty message" });
  assert.deepEqual(plain(list).map((o) => [o.id, o.state, o.refused ?? false]), [["a", "acked", false], ["b", "failed", true], ["c", "sending", false]]);
  // No ack in time: failed, but only from hosts that acknowledge at all.
  const empty = chat.emptyChat();
  assert.equal(chat.settleOutgoing(list, empty, chat.SEND_TIMEOUT_MS + 1, false), list);
  const late = chat.settleOutgoing(list, empty, chat.SEND_TIMEOUT_MS + 1, true);
  assert.deepEqual(plain(late).map((o) => [o.id, o.state, o.error ?? null]), [["a", "acked", null], ["b", "failed", "empty message"], ["c", "failed", "timeout"]]);
  const gone = chat.outgoingDisconnected([out("x"), out("y", "acked")]);
  assert.deepEqual(plain(gone).map((o) => [o.id, o.state]), [["x", "failed"], ["y", "acked"]]);
});

test("a message the full queue dropped fails; a retry keeps its id unless the host refused it", () => {
  seq = 0;
  const c = fold([ev(1, { t: "notice", level: "warn", code: "queue_full", text: "8 messages are already waiting", input_id: "q" })]);
  const [q] = plain(chat.settleOutgoing([out("q", "acked")], c, 1, true));
  assert.equal(q.state, "failed");
  assert.equal(q.refused, true);
  assert.equal(chat.retryOutgoing(q, "new", 5).id, "new");
  const lost = { ...out("l"), state: "failed", error: "disconnected" };
  const again = plain(chat.retryOutgoing(lost, "new", 5));
  assert.deepEqual([again.id, again.state, again.at], ["l", "sending", 5]);
});

test("requests are answered by the reply with their rid; lines are told apart", async () => {
  const sent = [];
  const r = chat.requester(async (c) => void sent.push(c), 50);
  const p = r.ask("files", { query: "src" });
  assert.deepEqual(plain(sent), [{ files: { query: "src", rid: "r1" } }]);
  assert.equal(r.reply({ t: "files", rid: "r9" }), false);
  assert.equal(r.reply({ t: "files", rid: "r1", files: ["a"] }), true);
  assert.deepEqual(plain(await p).files, ["a"]);
  await assert.rejects(r.ask("files", {}), /no answer/);
  assert.equal(chat.chatLineKind({ seq: 3, t: "message" }), "event");
  assert.equal(chat.chatLineKind({ t: "ack", input_id: "a" }), "reply");
  assert.equal(chat.chatLineKind({ t: "error", input_id: "a" }), "reply");
  assert.equal(chat.chatLineKind({ t: "files", rid: "r1" }), "reply");
  assert.equal(chat.chatLineKind({ t: "error", text: "x" }), "notice");
  assert.equal(chat.chatLineKind({ t: "bye" }), "closed");
  assert.equal(chat.chatLineKind({ pong: 3 }), null);
});

// ---------------------------------------------------------------------------------------------
// The / and @ menus

const input = load("chat-input.ts");

test("the caret's word opens the / menu at the start of the message and @ anywhere", () => {
  assert.deepEqual(plain(input.autocompleteAt("/rev", 4)), { mode: "slash", query: "rev", start: 0, end: 4 });
  assert.deepEqual(plain(input.autocompleteAt("  /", 3)), { mode: "slash", query: "", start: 2, end: 3 });
  // Not mid-message, and not a path.
  assert.equal(input.autocompleteAt("see /rev", 8), null);
  assert.equal(input.autocompleteAt("/usr/bin", 8), null);
  assert.deepEqual(plain(input.autocompleteAt("look at @src/ma and", 15)), { mode: "mention", query: "src/ma", start: 8, end: 15 });
  assert.equal(input.autocompleteAt("mail a@b", 8)?.mode, undefined);
  assert.equal(input.autocompleteAt("plain text", 5), null);
});

test("commands match by name first, then by description; choosing replaces the word", () => {
  const cmds = [
    { name: "compact", description: "Clear history but keep a summary", kind: "command", insert: "/compact" },
    { name: "review", description: "Review the changes", kind: "command", insert: "/review" },
    { name: "pr-comments", description: "Get the review comments", kind: "command", insert: "/pr-comments" },
    { name: "tidy", description: "a skill", kind: "skill", insert: "$tidy" },
  ];
  assert.deepEqual(plain(input.matchCommands(cmds, "re")).map((c) => c.name), ["review", "pr-comments"]);
  assert.deepEqual(plain(input.matchCommands(cmds, "")).length, 4);
  assert.deepEqual(plain(input.matchCommands(cmds, "zzz")), []);
  const ac = input.autocompleteAt("/re more", 3);
  assert.deepEqual(plain(input.applyChoice("/re more", ac, "/review")), { text: "/review more", caret: 8 });
  const at = input.autocompleteAt("fix @ma", 7);
  assert.deepEqual(plain(input.applyChoice("fix @ma", at, input.mentionText("src/main.rs"))), { text: "fix @src/main.rs ", caret: 17 });
  assert.equal(input.mentionText("docs/a b.md"), '@"docs/a b.md"');
});

test("files whose name matches come first; the menu wraps around", () => {
  assert.deepEqual(plain(input.rankFiles(["src/lib/main.rs", "docs/domain.md", "main.rs", "tests/x_main.rs"], "main")),
    ["main.rs", "src/lib/main.rs", "docs/domain.md", "tests/x_main.rs"]);
  assert.equal(input.moveActive(0, 3, -1), 2);
  assert.equal(input.moveActive(2, 3, 1), 0);
  assert.equal(input.moveActive(0, 0, 1), 0);
});

test("the agent's command list folds into the chat", () => {
  seq = 0;
  const c = fold([ev(0, { t: "commands", commands: [{ name: "review", kind: "command", insert: "/review" }] })]);
  assert.equal(c.commands[0].insert, "/review");
  assert.equal(c.turns.length, 0);
  assert.equal(chat.emptyChat().commands, null);
});

// ---------------------------------------------------------------------------------------------
// Images

test("images are checked against the host's limits and scaled to a long edge of 2048", () => {
  assert.equal(input.imageProblem("image/png", 1000, 0), null);
  assert.equal(input.imageProblem("image/svg+xml", 1000, 0), "type");
  assert.equal(input.imageProblem("image/jpeg", input.MAX_IMAGE_BYTES + 1, 0), "size");
  assert.equal(input.imageProblem("image/png", 10, input.MAX_IMAGES), "count");
  assert.deepEqual(plain(input.fitWithin(4000, 1000)), { width: 2048, height: 512, scaled: true });
  assert.deepEqual(plain(input.fitWithin(800, 600)), { width: 800, height: 600, scaled: false });
  assert.equal(input.base64Bytes("iVBORw0KGgo="), 8);
  assert.equal(input.base64Bytes("YWJj"), 3);
});

test("an input carries its images; the conversation keeps their names; this device keeps their pictures", () => {
  const img = { name: "a.png", mime: "image/png", data: "iVBORw0KGgo=" };
  assert.deepEqual(plain(chat.inputCommand("u1", "", undefined, [img])), { input: { id: "u1", text: "", attachments: [img] } });
  assert.deepEqual(plain(chat.inputCommand("u1", "hi", "queue", [])), { input: { id: "u1", text: "hi" } });
  seq = 0;
  const c = fold([
    ev(0, { t: "user", input_id: "u1", by: { device: "d" }, text: "", attachments: [{ name: "a.png", mime: "image/png", bytes: 8 }] }),
    ev(1, { t: "turn_start" }),
    ev(0, { t: "agent", images: false }),
  ]);
  const [row] = plain(chat.turnRows(c.turns[0]));
  assert.deepEqual([row.kind, row.inputId, row.attachments[0].name], ["user", "u1", "a.png"]);
  assert.equal(c.images, false);
  input.rememberImages("u1", [{ name: "a.png", url: "data:image/png;base64,iVBORw0KGgo=" }]);
  assert.equal(input.imagesOf("u1")[0].name, "a.png");
  assert.deepEqual(plain(input.imagesOf("nope")), []);
});
