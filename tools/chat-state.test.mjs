// How a coding-agent chat's events fold into what the chat panel shows (apps/desktop/src/lib/chat.ts),
// with the real module transpiled. Event shapes follow the host's chat/event.rs.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

function load() {
  const source = readFileSync(new URL("../apps/desktop/src/lib/chat.ts", import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const exports = {};
  const store = new Map();
  const localStorage = { getItem: (k) => store.get(k) ?? null, setItem: (k, v) => store.set(k, String(v)), removeItem: (k) => store.delete(k) };
  vm.runInNewContext(compiled, { exports, require: () => { throw new Error("no dependencies"); }, localStorage, JSON, Set, Map }, { filename: "chat.ts" });
  return exports;
}

const chat = load();
// Values from the module's own realm, as plain data for deep comparison.
const plain = (v) => JSON.parse(JSON.stringify(v));
let seq = 0;
const ev = (turn, e) => ({ seq: ++seq, ts: 1000 + seq * 100, turn, ...e });
const fold = (events) => events.reduce(chat.reduceChat, chat.emptyChat());

const conversation = () => {
  seq = 0;
  return [
    ev(0, { t: "state", state: "running", queued: [] }),
    ev(0, { t: "user", input_id: "u1", by: { device: "laptop" }, text: "fix the build" }),
    ev(1, { t: "turn_start" }),
    ev(1, { t: "message", id: "m:0", text: "Let me ", delta: true }),
    ev(1, { t: "message", id: "m:0", text: "look.", delta: true }),
    ev(1, { t: "tool_call", call_id: "k1", title: "Bash cargo build", kind: "execute", status: "in_progress" }),
    ev(1, { t: "thought", id: "r", text: "the import is missing", delta: true }),
    ev(1, { t: "tool_update", call_id: "k1", status: "failed", content: [{ type: "text", text: "error[E0425]" }] }),
    ev(1, { t: "tool_call", call_id: "k2", title: "Edit src/a.rs", kind: "edit", status: "completed", content: [{ type: "diff", path: "src/a.rs", old: "a\nb\nc", new: "a\nB\nc" }] }),
    ev(1, { t: "message", id: "m2:0", text: "Fixed: `b` was wrong.", delta: false }),
    ev(1, { t: "usage", input_tokens: 1200, output_tokens: 80 }),
    ev(1, { t: "turn_end", status: "completed", final_text: "Fixed", duration_ms: 4200 }),
    ev(1, { t: "state", state: "idle", queued: [] }),
  ];
};

test("a turn folds into its user message, streamed text, tools and its end", () => {
  const c = fold(conversation());
  assert.equal(c.state, "idle");
  assert.equal(c.turns.length, 1);
  const turn = c.turns[0];
  assert.equal(turn.user.text, "fix the build");
  assert.equal(turn.status, "completed");
  assert.equal(turn.durationMs, 4200);
  assert.equal(turn.blocks[0].text, "Let me look.");
  const tools = turn.blocks.filter((b) => b.kind === "tool");
  assert.deepEqual(plain(tools.map((t) => [t.callId, t.status])), [["k1", "failed"], ["k2", "completed"]]);
  assert.equal(tools[0].content[0].text, "error[E0425]");
});

test("a turn splits into preamble, folded steps and the answer", () => {
  const c = fold(conversation());
  const { preamble, steps, answer } = chat.splitTurn(c.turns[0]);
  assert.deepEqual(plain(preamble.map((b) => b.text)), ["Let me look."]);
  assert.deepEqual(plain(steps.map((b) => b.kind)), ["tool", "thought", "tool"]);
  assert.deepEqual(plain(answer.map((b) => b.text)), ["Fixed: `b` was wrong."]);
});

test("replayed events after a reconnect are not applied twice", () => {
  const events = conversation();
  const once = fold(events);
  const again = events.slice(3).reduce(chat.reduceChat, once);
  assert.equal(again, once, "an overlapping replay changes nothing");
  assert.equal(again.turns[0].blocks[0].text, "Let me look.");
});

test("a refused command shows as a notice without moving the cursor", () => {
  const c = fold(conversation());
  const after = chat.reduceChat(c, { seq: -1, ts: 0, turn: 0, t: "notice", level: "error", code: "readonly", text: "only watching" });
  assert.equal(after.seq, c.seq);
  assert.equal(after.notices.at(-1).text, "only watching");
});

test("tools still running when the turn ends take the turn's outcome; questions can be answered", () => {
  seq = 0;
  const c = fold([
    ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "go" }),
    ev(1, { t: "turn_start" }),
    ev(1, { t: "tool_call", call_id: "k", title: "Bash sleep 600", kind: "execute", status: "in_progress" }),
    ev(1, { t: "question", qid: "q1", prompt: "Which config?", options: ["debug", "release"] }),
    ev(1, { t: "turn_end", status: "interrupted", duration_ms: 900 }),
  ]);
  const tool = c.turns[0].blocks.find((b) => b.kind === "tool");
  assert.equal(tool.status, "failed");
  const answered = chat.markAnswered(c, "q1");
  assert.equal(answered.turns[0].blocks.find((b) => b.kind === "question").answered, true);
});

test("the phase follows the last thing the agent does", () => {
  seq = 0;
  let c = fold([
    ev(0, { t: "state", state: "running", queued: [] }),
    ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "go" }),
    ev(1, { t: "turn_start" }),
    ev(1, { t: "tool_call", call_id: "k", title: "Bash npm test", kind: "execute", status: "in_progress" }),
  ]);
  assert.equal(chat.phase(c).what, "tool");
  assert.equal(chat.phase(c).title, "Bash npm test");
  c = chat.reduceChat(c, ev(1, { t: "message", id: "m", text: "Done", delta: true }));
  assert.equal(chat.phase(c).what, "writing");
  c = chat.reduceChat(c, ev(1, { t: "state", state: "interrupting", queued: [] }));
  assert.equal(chat.phase(c).what, "stopping");
});

test("text splits into prose and fenced code, never markup", () => {
  const parts = chat.textParts("Run this:\n```sh\nnpm test\n```\n<b>not html</b> `x`");
  assert.deepEqual(plain(parts.map((p) => [p.code, p.text.trim()])), [[false, "Run this:"], [true, "npm test"], [false, "<b>not html</b> `x`"]]);
  assert.equal(parts[1].lang, "sh");
  assert.deepEqual(plain(chat.inlineParts("a `b` c")), [{ code: false, text: "a " }, { code: true, text: "b" }, { code: false, text: " c" }]);
});

test("diffs keep context and count changed lines", () => {
  const lines = chat.diffLines("a\nb\nc\nd", "a\nB\nc\nd");
  assert.deepEqual(plain(lines.map((l) => l.sign + l.text)), [" a", "-b", "+B", " c", " d"]);
  assert.deepEqual(plain(chat.diffStat([{ type: "diff", path: "x", old: null, new: "1\n2" }])), { add: 2, del: 0 });
  assert.equal(chat.diffStat([{ type: "text", text: "x" }]), null);
});

test("recent folders are remembered per computer, newest first", () => {
  chat.rememberCwd("office-pc", "D:\\a");
  chat.rememberCwd("office-pc", "D:\\b");
  chat.rememberCwd("office-pc", "D:\\a");
  assert.deepEqual(plain(chat.recentCwds("office-pc")), ["D:\\a", "D:\\b"]);
  assert.equal(chat.lastCwd("lab-pc"), "");
});
