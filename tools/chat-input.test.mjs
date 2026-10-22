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
