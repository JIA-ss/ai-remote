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

test("a turn projects into rows: tools and edits in one run, the reply last", () => {
  const c = fold(conversation());
  const rows = chat.turnRows(c.turns[0]);
  // As in Orca, an edit is part of the run ("ran 1 command and edited 1 file") and opens to its diff
  // card; the thought between the build and the edit stays inside the run.
  assert.deepEqual(plain(rows.map((r) => r.kind)), ["user", "text", "run", "text"]);
  assert.deepEqual(plain(rows[2].members.map((m) => m.kind)), ["tool", "thought", "tool"]);
  assert.deepEqual(plain(chat.runSummary(rows[2].tools)), [{ cat: "command", n: 1 }, { cat: "change", n: 1 }]);
  assert.deepEqual(plain(chat.runDiffStat(rows[2].tools)), { add: 1, del: 1 });
  assert.equal(chat.runDiffStat([rows[2].tools[0]]), null);
  assert.equal(rows[2].live, false);
  assert.equal(rows[3].text, "Fixed: `b` was wrong.");
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
  // Stopped, not failed: the run does not count it as a failure.
  assert.equal(tool.stopped, true);
  assert.deepEqual(plain(chat.runOutcome([tool])), { failed: 0, stopped: 1, done: true });
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

test("recent folders are remembered per computer, newest first", () => {
  chat.rememberCwd("office-pc", "D:\\a");
  chat.rememberCwd("office-pc", "D:\\b");
  chat.rememberCwd("office-pc", "D:\\a");
  assert.deepEqual(plain(chat.recentCwds("office-pc")), ["D:\\a", "D:\\b"]);
  assert.equal(chat.lastCwd("lab-pc"), "");
});

test("tool calls in a row form one run; reasoning between them stays inside; a running turn's run is live", () => {
  seq = 0;
  const c = fold([
    ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "go" }),
    ev(1, { t: "turn_start" }),
    ev(1, { t: "tool_call", call_id: "a", title: "Read src/a.rs", kind: "read", status: "completed" }),
    ev(1, { t: "thought", id: "r", text: "now the tests", delta: false }),
    ev(1, { t: "tool_call", call_id: "b", title: "Bash cargo test", kind: "execute", status: "completed" }),
    ev(1, { t: "tool_call", call_id: "c", title: "Bash cargo build", kind: "execute", status: "in_progress" }),
  ]);
  const rows = chat.turnRows(c.turns[0]);
  assert.deepEqual(plain(rows.map((r) => r.kind)), ["user", "run"]);
  assert.deepEqual(plain(rows[1].members.map((m) => m.kind)), ["tool", "thought", "tool", "tool"]);
  assert.equal(rows[1].live, true);
  // The sentence counts each kind once, in the order it first appears; singular and plural are separate keys.
  assert.deepEqual(plain(chat.runSummary(rows[1].tools)), [{ cat: "read", n: 1 }, { cat: "command", n: 2 }]);
  assert.deepEqual(plain(chat.runOutcome(rows[1].tools)), { failed: 0, stopped: 0, done: false });
});

test("a sub-agent (the Task tool) gets a row of its own", () => {
  seq = 0;
  const c = fold([
    ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "go" }),
    ev(1, { t: "tool_call", call_id: "s", title: "Task Find the date helpers", kind: "think", status: "in_progress" }),
  ]);
  assert.deepEqual(plain(chat.turnRows(c.turns[0]).map((r) => r.kind)), ["user", "agent"]);
});

test("a message added to the running turn (steer) shows inside it, flagged", () => {
  seq = 0;
  let c = fold([
    ev(0, { t: "state", state: "running", queued: [] }),
    ev(0, { t: "user", input_id: "u1", by: { device: "laptop" }, text: "fix the build" }),
    ev(1, { t: "turn_start" }),
    ev(1, { t: "tool_call", call_id: "k", title: "Bash cargo build", kind: "execute", status: "in_progress" }),
    // The host numbers a steer with the running turn.
    ev(1, { t: "user", input_id: "u2", by: { device: "laptop" }, text: "use the release profile", steer: true }),
    ev(1, { t: "message", id: "m", text: "Switching to release.", delta: false }),
  ]);
  assert.equal(c.turns.length, 1);
  const rows = chat.turnRows(c.turns[0]);
  assert.deepEqual(plain(rows.map((r) => [r.kind, r.steer ?? null])), [["user", false], ["run", null], ["user", true], ["text", null]]);
  assert.equal(rows[2].text, "use the release profile");
  // An older host might number it as the next turn: it still lands in the running one.
  c = chat.reduceChat(c, ev(1, { t: "user", input_id: "u3", by: { device: "laptop" }, text: "and skip docs", steer: true, turn: 2 }));
  assert.equal(c.turns.length, 1);
  assert.equal(c.turns[0].blocks.at(-1).text, "and skip docs");
  // A host without steering queues it and says so.
  c = chat.reduceChat(c, ev(1, { t: "notice", level: "info", code: "steer_queued", text: "queued" }));
  assert.equal(c.turns[0].blocks.at(-1).code, "steer_queued");
});

test("the composer sends steer, queue or interrupt as the agent allows", () => {
  assert.equal(chat.sendMode(false, { steer: true }), undefined);
  assert.equal(chat.sendMode(true, { steer: true }), "steer");
  assert.equal(chat.sendMode(true, { steer: false }), "queue");
  assert.equal(chat.sendMode(true, null), "queue");
  assert.equal(chat.sendMode(true, { steer: true }, true), "interrupt");
  assert.deepEqual(plain(chat.inputCommand("i1", "hi", "steer")), { input: { id: "i1", text: "hi", mode: "steer" } });
  // Queue is the host's default: the field is left out, so older hosts read it unchanged.
  assert.deepEqual(plain(chat.inputCommand("i2", "hi", "queue")), { input: { id: "i2", text: "hi" } });
});

test("editing a queued message takes the old one back and queues the new text", () => {
  assert.deepEqual(plain(chat.editQueued("q1", "q2", "new text")), [{ dequeue: { id: "q1" } }, { input: { id: "q2", text: "new text" } }]);
});

test("↑ and ↓ walk through sent messages and back to the draft", () => {
  let h = chat.rememberSent([], "one");
  h = chat.rememberSent(h, "two");
  h = chat.rememberSent(h, "one");
  assert.deepEqual(plain(h), ["two", "one"]);
  const up = chat.browseHistory(h, h.length, -1, "draft");
  assert.deepEqual(plain(up), { at: 1, text: "one" });
  const up2 = chat.browseHistory(h, up.at, -1, "draft");
  assert.deepEqual(plain(up2), { at: 0, text: "two" });
  assert.equal(chat.browseHistory(h, 0, -1, "draft"), null);
  assert.deepEqual(plain(chat.browseHistory(h, 1, 1, "draft")), { at: 2, text: "draft" });
});

test("usage with a context window feeds the ring; usage without it leaves it alone", () => {
  seq = 0;
  let c = fold([ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "go" }), ev(1, { t: "usage", input_tokens: 10, output_tokens: 2, context_window: 200000, context_used: 45000 })]);
  assert.deepEqual(plain(c.context), { used: 45000, window: 200000 });
  c = chat.reduceChat(c, ev(1, { t: "usage", input_tokens: 12 }));
  assert.deepEqual(plain(c.context), { used: 45000, window: 200000 });
  assert.equal(c.turns[0].usage.input, 12);
});

test("a stopped turn offers to continue until something else happens", () => {
  seq = 0;
  let c = fold([
    ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "go" }),
    ev(1, { t: "turn_end", status: "interrupted", duration_ms: 900 }),
    ev(1, { t: "state", state: "idle", queued: [] }),
  ]);
  assert.equal(chat.stoppedTurn(c).n, 1);
  c = chat.reduceChat(c, ev(1, { t: "state", state: "running", queued: [] }));
  assert.equal(chat.stoppedTurn(c), null);
});

test("answers are kept with their question", () => {
  seq = 0;
  const c = fold([ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "go" }), ev(1, { t: "question", qid: "q", prompt: "Which?", options: ["a", "b"] })]);
  const after = chat.markAnswered(c, "q", "b");
  const q = after.turns[0].blocks[0];
  assert.equal(q.answered, true);
  assert.equal(q.answer, "b");
});

test("diffs show changes with context, fold unchanged stretches, and count lines", () => {
  const old = Array.from({ length: 20 }, (_, i) => `line ${i}`).join("\n");
  const neu = old.replace("line 2\n", "line two\n").replace("line 17", "line seventeen");
  const rows = chat.diffRows(old, neu);
  const kinds = plain(rows.map((r) => r.kind));
  // Two separate changes, each with three lines around it, and the stretch between folded.
  assert.deepEqual(kinds.filter((k) => k !== "context"), ["del", "add", "gap", "del", "add"]);
  assert.equal(rows.find((r) => r.kind === "gap").lines, 8);
  // Lines before the first and after the last change are left out without a marker.
  const mid = chat.diffRows(old, old.replace("line 10", "line ten"));
  assert.notEqual(mid[0].kind, "gap");
  assert.notEqual(mid.at(-1).kind, "gap");
  assert.equal(mid.length, 8);
  assert.deepEqual(plain(chat.diffStat([{ type: "diff", path: "x", old, new: neu }])), { add: 2, del: 2 });
  // A new file is all added; a change in the middle is a real line diff, not "everything replaced".
  assert.deepEqual(plain(chat.diffStat([{ type: "diff", path: "x", old: null, new: "1\n2" }])), { add: 2, del: 0 });
  assert.deepEqual(plain(chat.diffStat([{ type: "diff", path: "x", old: "a\nb\nc\nd\ne", new: "a\nB\nc\nD\ne" }])), { add: 2, del: 2 });
  assert.equal(chat.diffStat([{ type: "text", text: "x" }]), null);
  assert.match(chat.patchText("src/a.rs", chat.diffRows("a", "b")), /^--- src\/a.rs\n\+\+\+ src\/a.rs\n-a\n\+b$/);
});

test("long output shows its tail; tool labels split verb from target", () => {
  const out = Array.from({ length: 30 }, (_, i) => `l${i}`).join("\n");
  const tail = chat.tailLines(out, 14);
  assert.equal(tail.hidden, 16);
  assert.equal(tail.text.split("\n")[0], "l16");
  const x = { kind: "tool", callId: "k", title: "Bash npm test -- --watch", tool: "execute", status: "completed", input: { command: "npm test -- --watch" }, locations: [], content: [], truncated: false, startedTs: 0, endedTs: 1 };
  assert.deepEqual(plain(chat.toolLabel(x)), { verb: "Bash", target: "npm test -- --watch" });
  assert.equal(chat.toolCommand(x), "npm test -- --watch");
  assert.equal(chat.baseName("D:\\work\\export.py"), "export.py");
});

test("a session the host no longer has is recognized, other errors are not", () => {
  assert.equal(chat.sessionGone("no chat session 'c3f9' on this host"), true);
  assert.equal(chat.sessionGone("'chat_attach' requires level 'admin'"), false);
  assert.equal(chat.sessionGone("connection lost"), false);
});

// ---- The workspace's protocol additions (docs/agent-workspace.zh-CN.md §1): titles, Claude's
// permission mode, permissions and plans to answer, questions in parts, `resolved`, diff hunks.

const claudeTurn = () => {
  seq = 0;
  return [
    ev(0, { t: "state", state: "running", queued: [] }),
    ev(0, { t: "user", input_id: "u1", by: { device: "laptop" }, text: "build it" }),
    ev(1, { t: "turn_start" }),
    ev(1, { t: "tool_call", call_id: "k1", title: "Bash cargo build", kind: "execute", status: "in_progress", input: { command: "cargo build" } }),
  ];
};

test("a title and the permission mode are the session's, not a turn's", () => {
  seq = 0;
  let c = fold([ev(0, { t: "mode", mode: "default" }), ev(0, { t: "title", title: "Fix the build", source: "agent" })]);
  assert.deepEqual(plain(c.title), { text: "Fix the build", source: "agent" });
  assert.equal(c.mode, "default");
  assert.equal(c.turns.length, 0, "neither starts a turn");
  c = chat.reduceChat(c, ev(0, { t: "mode", mode: "acceptEdits" }));
  c = chat.reduceChat(c, ev(0, { t: "title", title: "Mine" }));
  assert.equal(c.mode, "acceptEdits");
  assert.deepEqual(plain(c.title), { text: "Mine", source: "agent" }, "a title without a source counts as the agent's");
  assert.deepEqual(plain([chat.emptyChat().title, chat.emptyChat().mode, chat.emptyChat().requests]), [null, null, []]);
});

test("a permission waits beside its turn and leaves the turn's rows as they were", () => {
  const before = fold(claudeTurn());
  const c = chat.reduceChat(before, ev(1, { t: "permission", rid: "p1", tool: "Bash", kind: "execute", title: "cargo build", input: { command: "cargo build" },
    reason: "not allowed by the rules", suggestions: ["session"], call_id: "k1" }));
  assert.equal(c.turns[0], before.turns[0], "the turn object is untouched (no re-render)");
  assert.deepEqual(plain(c.requests.map((r) => [r.kind, r.id, r.turn, r.tool, r.toolKind, r.callId, r.suggestions, r.resolved])),
    [["permission", "p1", 1, "Bash", "execute", "k1", ["session"], null]]);
  assert.deepEqual(plain(chat.openRequests(c).map((r) => r.id)), ["p1"]);
  // The same request again (a replay) is not a second one.
  const again = chat.reduceChat(c, ev(1, { t: "permission", rid: "p1", tool: "Bash" }));
  assert.equal(again.requests.length, 1);
});

test("resolved settles a request; the first answer wins over this device's own mark", () => {
  let c = fold([...claudeTurn(), ev(1, { t: "permission", rid: "p1", tool: "Bash", suggestions: [] }), ev(1, { t: "plan_review", rid: "p2", plan: "## Plan\n1. build" })]);
  assert.deepEqual(plain(chat.openRequests(c).map((r) => [r.kind, r.id])), [["permission", "p1"], ["plan", "p2"]]);
  // This device answered: the card leaves at once.
  c = chat.markResolved(c, "p1", "allowed", 5000);
  assert.deepEqual(plain(c.requests[0].resolved), { outcome: "allowed", ts: 5000 });
  assert.deepEqual(plain(chat.openRequests(c).map((r) => r.id)), ["p2"]);
  // Another device was first: the host's word replaces ours.
  c = chat.reduceChat(c, ev(1, { t: "resolved", id: "p1", outcome: "denied", by: { device: "phone" } }));
  assert.equal(c.requests[0].resolved.outcome, "denied");
  assert.equal(c.requests[0].resolved.by, "phone");
  // A local mark after the host settled it changes nothing.
  assert.equal(chat.markResolved(c, "p1", "allowed_session"), c);
  c = chat.reduceChat(c, ev(1, { t: "resolved", id: "p2", outcome: "allowed" }));
  assert.equal(chat.openRequests(c).length, 0);
  assert.equal(chat.markResolved(c, "nope", "denied"), c, "an unknown id is ignored");
});

test("what a turn still asked about falls away when it ends", () => {
  let c = fold([...claudeTurn(), ev(1, { t: "permission", rid: "p1", tool: "Bash" }), ev(1, { t: "plan_review", rid: "p2", plan: "x" })]);
  c = chat.reduceChat(c, ev(1, { t: "resolved", id: "p2", outcome: "allowed" }));
  c = chat.reduceChat(c, ev(1, { t: "turn_end", status: "interrupted", duration_ms: 10 }));
  assert.deepEqual(plain(c.requests.map((r) => [r.id, r.resolved.outcome])), [["p1", "cancelled"], ["p2", "allowed"]]);
  // The host's own `resolved cancelled` afterwards is fine too.
  c = chat.reduceChat(c, ev(1, { t: "resolved", id: "p1", outcome: "cancelled" }));
  assert.equal(c.requests[0].resolved.outcome, "cancelled");
});

test("a question in parts keeps its parts; resolved answers it, cancelled does not", () => {
  const parts = [
    { header: "Config", question: "Which config?", options: [{ label: "debug" }, { label: "release", description: "optimized" }] },
    { question: "Which targets?", options: [{ label: "linux" }, { label: "mac" }], multi: true },
  ];
  const c = fold([...claudeTurn(), ev(1, { t: "question", qid: "q1", prompt: "Which config?", options: ["debug", "release"], questions: parts })]);
  const q = c.turns[0].blocks.find((b) => b.kind === "question");
  assert.deepEqual(plain(q.questions), parts);
  assert.equal(typeof q.seq, "number");
  assert.deepEqual(plain(chat.openRequests(c).map((r) => [r.kind, r.qid, r.turn])), [["question", "q1", 1]]);
  // Answered here: the parts' answers are kept for the receipt.
  const mine = chat.markAnswered(c, "q1", "release", { "Which config?": "release", "Which targets?": "linux, mac" });
  assert.deepEqual(plain(mine.turns[0].blocks.find((b) => b.kind === "question").partAnswers), { "Which config?": "release", "Which targets?": "linux, mac" });
  assert.equal(chat.openRequests(mine).length, 0);
  // Answered on another device.
  const there = chat.reduceChat(c, ev(1, { t: "resolved", id: "q1", outcome: "answered", by: { device: "phone" } }));
  const tq = there.turns[0].blocks.find((b) => b.kind === "question");
  assert.deepEqual(plain([tq.answered, tq.outcome]), [true, "answered"]);
  // Cancelled: no longer open, but not shown as answered either.
  const gone = chat.reduceChat(c, ev(1, { t: "resolved", id: "q1", outcome: "cancelled" }));
  const gq = gone.turns[0].blocks.find((b) => b.kind === "question");
  assert.deepEqual(plain([gq.answered, gq.outcome]), [false, "cancelled"]);
  assert.equal(chat.openRequests(gone).length, 0);
});

test("an older host's question (no parts) keeps its old shape and is open while its turn runs", () => {
  const c = fold([...claudeTurn(), ev(1, { t: "question", qid: "q", prompt: "Which?", options: ["a"] })]);
  const q = c.turns[0].blocks.find((b) => b.kind === "question");
  assert.deepEqual(plain(Object.keys(q).sort()), ["answered", "kind", "options", "prompt", "qid"]);
  assert.equal(chat.openRequests(c).length, 1);
  const ended = chat.reduceChat(c, ev(1, { t: "turn_end", status: "completed", duration_ms: 1 }));
  assert.equal(chat.openRequests(ended).length, 0, "a finished turn waits for nothing");
});

test("a diff can carry hunks with real line numbers; the rest of the panel reads it as before", () => {
  const hunks = [{ old_start: 40, old_lines: 3, new_start: 40, new_lines: 4, lines: [" a", "-b", "+B", "+C", " d"] }];
  const c = fold([...claudeTurn(), ev(1, { t: "tool_call", call_id: "e1", title: "Edit src/a.rs", kind: "edit", status: "in_progress",
    content: [{ type: "diff", path: "src/a.rs", old: "b", new: "B\nC" }] }),
    ev(1, { t: "tool_update", call_id: "e1", status: "completed", content: [{ type: "diff", path: "src/a.rs", old: "b", new: "B\nC", hunks }] })]);
  const e = c.turns[0].blocks.find((b) => b.kind === "tool" && b.callId === "e1");
  assert.deepEqual(plain(e.content.at(-1).hunks), hunks);
  assert.deepEqual(plain(chat.diffStat([e.content[0]])), { add: 2, del: 1 });
});

test("answer, permit and mode commands are what the host reads", () => {
  assert.deepEqual(plain(chat.permitCommand("p3", "once")), { permit: { rid: "p3", decision: "once" } });
  assert.deepEqual(plain(chat.permitCommand("p3", "deny", { message: "  not that folder " })), { permit: { rid: "p3", decision: "deny", message: "not that folder" } });
  assert.deepEqual(plain(chat.permitCommand("p4", "once", { mode: "acceptEdits", message: " " })), { permit: { rid: "p4", decision: "once", mode: "acceptEdits" } });
  assert.deepEqual(plain(chat.modeCommand("plan")), { mode: { mode: "plan" } });
  assert.deepEqual(plain(chat.answerCommand("q2", "release")), { answer: { qid: "q2", text: "release" } });
  assert.deepEqual(plain(chat.answerCommand("q2", "release", { "Which?": "release" })), { answer: { qid: "q2", text: "release", answers: { "Which?": "release" } } });
});

test("an edit's update with the real patch replaces the call's diff of that file, output is added", () => {
  seq = 0;
  const c = fold([
    ev(1, { t: "turn_start" }),
    ev(1, { t: "tool_call", call_id: "e1", title: "Edit src/a.rs", kind: "edit", status: "in_progress", content: [{ type: "diff", path: "src/a.rs", old: "b", new: "B" }] }),
    ev(1, { t: "tool_update", call_id: "e1", status: "completed", content: [{ type: "diff", path: "src/a.rs", old: "a\nb\nc", new: "a\nB\nc", hunks: [{ old_start: 22, new_start: 22 }] }, { type: "text", text: "ok" }] }),
  ]);
  const tool = c.turns[0].blocks.find((b) => b.kind === "tool");
  assert.deepEqual(plain(tool.content.map((x) => x.type)), ["diff", "text"]);
  assert.equal(tool.content[0].hunks[0].old_start, 22);
  assert.deepEqual(plain(chat.mergeToolContent([{ type: "text", text: "a" }], [{ type: "text", text: "b" }]).map((x) => x.text)), ["a", "b"]);
});
