// How a Codex conversation is laid out (apps/desktop/src/lib/codex.ts): commands read as what they do,
// diffs with line numbers, and a turn's work folded into units, its answer and its changes. The real
// modules are transpiled; event shapes follow the host's chat/event.rs and adapters/codex.rs.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

function compile(name) {
  const source = readFileSync(new URL(`../apps/desktop/src/lib/${name}.ts`, import.meta.url), "utf8");
  return ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
}

function load() {
  const modules = {};
  const run = (name) => {
    if (modules[name]) return modules[name];
    const exports = {};
    modules[name] = exports;
    const require = (spec) => {
      const m = /^\.\/(\w[\w-]*)$/.exec(spec);
      if (!m) throw new Error(`no dependency ${spec}`);
      return run(m[1]);
    };
    vm.runInNewContext(compile(name), { exports, require, JSON, Set, Map, Math, Number, String, Array, Object, RegExp }, { filename: `${name}.ts` });
    return exports;
  };
  return { chat: run("chat"), codex: run("codex") };
}

const { chat, codex } = load();
const plain = (v) => JSON.parse(JSON.stringify(v));
let seq = 0;
const ev = (turn, e) => ({ seq: ++seq, ts: 1000 + seq * 100, turn, ...e });
const fold = (events) => events.reduce(chat.reduceChat, chat.emptyChat());

test("the shell Codex wraps a command in is left out", () => {
  assert.equal(codex.unwrapShell("/bin/zsh -lc 'ls -la'"), "ls -la");
  assert.equal(codex.unwrapShell('bash -lc "rg -n \\"a b\\" src"'), 'rg -n "a b" src');
  assert.equal(codex.unwrapShell("/bin/bash -l -c 'npm test'"), "npm test");
  assert.equal(codex.unwrapShell('"C:\\\\Windows\\\\System32\\\\WindowsPowerShell\\\\v1.0\\\\powershell.exe" -Command Get-ChildItem src'), "Get-ChildItem src");
  assert.equal(codex.unwrapShell("cmd.exe /c dir"), "dir");
  assert.equal(codex.unwrapShell("git status"), "git status");
  assert.equal(codex.unwrapShell("bash run.sh x"), "bash run.sh x");
});

test("commands read as reads, searches and listings where they are only that", () => {
  const p = (c) => plain(codex.parseCommand(c));
  assert.deepEqual(p("sed -n '1,120p' src/app.ts"), [{ type: "read", path: "src/app.ts", name: "app.ts" }]);
  assert.deepEqual(p("cat a.txt b/c.md"), [{ type: "read", path: "a.txt", name: "a.txt" }, { type: "read", path: "b/c.md", name: "c.md" }]);
  assert.deepEqual(p("nl -ba src/x.rs | sed -n '1,40p'"), [{ type: "read", path: "src/x.rs", name: "x.rs" }]);
  assert.deepEqual(p("head -n 50 README.md"), [{ type: "read", path: "README.md", name: "README.md" }]);
  assert.deepEqual(p('rg -n "quarter_end" src -g "*.py"'), [{ type: "search", query: "quarter_end", path: "src" }]);
  assert.deepEqual(p("grep -rn TODO ."), [{ type: "search", query: "TODO", path: "." }]);
  assert.deepEqual(p("rg --files src | head -50"), [{ type: "list_files", path: "src" }]);
  assert.deepEqual(p("ls -la apps"), [{ type: "list_files", path: "apps" }]);
  assert.deepEqual(p("find . -name '*.toml'"), [{ type: "search", query: "*.toml", path: "." }]);
  // Echoes between reads are not commands of their own.
  assert.deepEqual(p("sed -n '1,20p' a.rs && echo --- && sed -n '1,20p' b.rs").map((a) => a.path), ["a.rs", "b.rs"]);
  // Anything else makes it a command.
  assert.deepEqual(p("cargo test -p x"), [{ type: "unknown" }]);
  assert.deepEqual(p("cat a.txt && npm run build"), [{ type: "unknown" }]);
  assert.deepEqual(p("sed -i 's/a/b/' x"), [{ type: "unknown" }]);
});

test("a command's info comes from the host, or from its title and output on older hosts", () => {
  const base = { kind: "tool", callId: "c", tool: "execute", status: "completed", locations: [], truncated: false, startedTs: 1000, endedTs: 3400 };
  const fresh = codex.execInfo({ ...base, title: "Shell rg x", input: { command: "rg x", cwd: "/w", actions: [{ type: "search", command: "rg x", query: "x", path: null }] },
    content: [{ type: "text", text: "a.rs:1:x\n" }], exitCode: 0, durationMs: 1234 });
  assert.equal(fresh.command, "rg x");
  assert.equal(fresh.explore, true);
  assert.deepEqual(plain(fresh.actions), [{ type: "search", query: "x", path: null }]);
  assert.equal(fresh.durationMs, 1234);
  const streamed = codex.execInfo({ ...base, title: "Shell seq 3", content: [{ type: "text", text: "1\n2" }, { type: "text", text: "\n3\n" }] });
  assert.equal(streamed.output, "1\n2\n3\n", "streamed pieces join as they came");
  const old = codex.execInfo({ ...base, status: "failed", title: "/bin/zsh -lc 'cargo build'", content: [{ type: "text", text: "error\n" }, { type: "text", text: "(exit code 101)" }] });
  assert.equal(old.command, "cargo build");
  assert.equal(old.explore, false);
  assert.equal(old.exitCode, 101);
  assert.equal(old.output.trim(), "error");
  assert.equal(old.durationMs, 2400);
});

test("a unified diff keeps its line numbers; an added file's content is all added", () => {
  const d = codex.parseUnifiedDiff("@@ -10,3 +10,4 @@ fn x\n a\n-b\n+B\n+C\n c\n");
  assert.equal(d.add, 2);
  assert.equal(d.del, 1);
  assert.deepEqual(plain(d.lines.map((l) => [l.kind, l.old ?? null, l.new ?? null])), [
    ["hunk", null, null], ["context", 10, 10], ["del", 11, null], ["add", null, 11], ["add", null, 12], ["context", 12, 13],
  ]);
  const added = codex.parseUnifiedDiff("one\ntwo\n", "add");
  assert.equal(added.add, 2);
  assert.equal(added.lines[1].new, 2);
});

test("an edit's files come from the host's list, or are merged per path from its hunks", () => {
  const base = { kind: "tool", callId: "f", title: "edit a", tool: "edit", status: "completed", locations: [], truncated: false, startedTs: 0, endedTs: 1 };
  const fresh = codex.fileChanges({ ...base, input: { files: [{ path: "/w/a.ts", kind: "update", diff: "@@ -1 +1 @@\n-x\n+y\n" }, { path: "/w/n.ts", kind: "add", diff: "hi\n" }] }, content: [] });
  assert.deepEqual(plain(fresh.map((f) => [f.path, f.kind, f.add, f.del])), [["/w/a.ts", "update", 1, 1], ["/w/n.ts", "add", 1, 0]]);
  const old = codex.fileChanges({ ...base, content: [
    { type: "diff", path: "a.ts", old: "1\n2\n", new: "1\nX\n" }, { type: "diff", path: "a.ts", old: "9\n", new: "9\n10\n" }, { type: "diff", path: "b.ts", old: null, new: "z\n" },
  ] });
  assert.deepEqual(plain(old.map((f) => [f.path, f.kind, f.add, f.del])), [["a.ts", "update", 2, 1], ["b.ts", "add", 1, 0]]);
  assert.ok(old[0].lines.some((l) => l.kind === "hunk"), "the two hunks stay apart");
});

const codexTurn = () => {
  seq = 0;
  return [
    ev(0, { t: "state", state: "running", queued: [] }),
    ev(0, { t: "user", input_id: "u1", by: { device: "mac" }, text: "fix the export" }),
    ev(1, { t: "turn_start" }),
    ev(1, { t: "thought", id: "r1", text: "**Looking at the export**", delta: true }),
    ev(1, { t: "message", id: "m1", text: "I'll look at the export first.", delta: true, phase: "commentary" }),
    ev(1, { t: "tool_call", call_id: "c1", title: "Shell rg -n quarter_end", kind: "execute", status: "in_progress",
      input: { command: "rg -n quarter_end", cwd: "/w", actions: [{ type: "search", query: "quarter_end", path: null }] } }),
    ev(1, { t: "tool_update", call_id: "c1", status: "completed", content: [{ type: "text", text: "export.py:1" }], exit_code: 0, duration_ms: 80 }),
    ev(1, { t: "tool_call", call_id: "c2", title: "Shell sed -n '1,40p' export.py", kind: "execute", status: "in_progress",
      input: { command: "sed -n '1,40p' export.py", cwd: "/w", actions: [{ type: "read", name: "export.py", path: "/w/export.py" }] } }),
    ev(1, { t: "tool_update", call_id: "c2", status: "completed", exit_code: 0 }),
    ev(1, { t: "thought", id: "r2", text: "day 31 is wrong", delta: true }),
    ev(1, { t: "tool_call", call_id: "f1", title: "edit /w/export.py", kind: "edit", status: "in_progress",
      input: { files: [{ path: "/w/export.py", kind: "update", diff: "@@ -1,2 +1,2 @@\n def q():\n-    return 31\n+    return last_day()\n" }] },
      content: [{ type: "diff", path: "/w/export.py", old: " def q():\n    return 31\n", new: " def q():\n    return last_day()\n" }] }),
    ev(1, { t: "tool_update", call_id: "f1", status: "completed" }),
    ev(1, { t: "tool_call", call_id: "c3", title: "Shell pytest -q", kind: "execute", status: "in_progress", input: { command: "pytest -q", cwd: "/w", actions: [{ type: "unknown" }] } }),
  ];
};

test("a running turn: one group for the tool calls, the remark before it, nothing folded yet", () => {
  const c = fold(codexTurn());
  const v = codex.codexTurn(c.turns[0]);
  assert.deepEqual(plain(v.units.map((u) => u.kind)), ["thought", "message", "group"]);
  const g = v.units[2];
  assert.equal(g.live, true);
  assert.deepEqual(plain(g.acts.map((a) => a.kind)), ["exec", "exec", "thought", "patch", "exec"]);
  assert.equal(codex.activeAct(g.acts).tool.callId, "c3");
  assert.equal(v.answer, null);
  assert.equal(v.foldable, false);
  assert.equal(v.status, "working");
  assert.equal(v.thinking, false, "the live group says it is working");
  const s = codex.groupSummary(g.acts);
  assert.deepEqual(plain(s.parts), [{ part: "edited", n: 1 }, { part: "read", n: 1 }, { part: "ran", n: 1 }]);
  assert.deepEqual([s.add, s.del], [1, 1]);
});

test("the answer folds the work away; the turn's changes close it", () => {
  const events = [
    ...codexTurn(),
    ev(1, { t: "tool_update", call_id: "c3", status: "completed", content: [{ type: "text", text: "3 passed" }], exit_code: 0, duration_ms: 2300 }),
    ev(1, { t: "message", id: "m2", text: "Fixed: ", delta: true, phase: "final_answer" }),
  ];
  const streaming = codex.codexTurn(fold(events).turns[0]);
  assert.equal(streaming.answer.block.id, "m2");
  assert.equal(streaming.answer.streaming, true);
  assert.equal(streaming.foldable, true);
  assert.equal(streaming.units.at(-1).live, false, "the work is done once the answer runs");
  const done = fold([...events, ev(1, { t: "message", id: "m2", text: "the last day.", delta: true, phase: "final_answer" }),
    ev(1, { t: "turn_end", status: "completed", final_text: "Fixed", duration_ms: 83_000 })]);
  const v = codex.codexTurn(done.turns[0]);
  assert.equal(v.status, "worked");
  assert.equal(v.answer.block.text, "Fixed: the last day.");
  assert.equal(v.answer.streaming, false);
  assert.deepEqual(plain(v.changes.map((f) => [f.path, f.add, f.del])), [["/w/export.py", 1, 1]]);
  assert.equal(codex.workedMs(done.turns[0], 0), 83_000);
  assert.equal(codex.duration(83_000), "1m 23s");
  assert.equal(codex.duration(83_000, true), "1 分 23 秒");
});

test("a stopped turn does not fold; a commentary is never the answer", () => {
  const stopped = fold([...codexTurn(), ev(1, { t: "message", id: "m2", text: "Partial", delta: true, phase: "final_answer" }),
    ev(1, { t: "turn_end", status: "interrupted", duration_ms: 5000 })]);
  const v = codex.codexTurn(stopped.turns[0]);
  assert.equal(v.status, "stopped");
  assert.equal(v.foldable, false);
  const remark = fold([...codexTurn().slice(0, 5), ev(1, { t: "turn_end", status: "completed", duration_ms: 1000 })]);
  assert.equal(codex.codexTurn(remark.turns[0]).answer, null);
});

test("a host that does not send phases: the last text of a finished turn is its answer", () => {
  seq = 0;
  const c = fold([
    ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "hi" }),
    ev(1, { t: "turn_start" }),
    ev(1, { t: "message", id: "a", text: "Checking.", delta: false }),
    ev(1, { t: "tool_call", call_id: "c", title: "/bin/zsh -lc ls", kind: "execute", status: "completed" }),
    ev(1, { t: "message", id: "b", text: "Done.", delta: false }),
  ]);
  assert.equal(codex.codexTurn(c.turns[0]).answer, null, "still running: it may be a remark");
  const done = chat.reduceChat(c, ev(1, { t: "turn_end", status: "completed", duration_ms: 900 }));
  const v = codex.codexTurn(done.turns[0]);
  assert.equal(v.answer.block.id, "b");
  assert.deepEqual(plain(v.units.map((u) => u.kind)), ["message", "group"]);
  const g = v.units[1];
  assert.equal(g.acts[0].info.command, "ls");
  assert.equal(g.acts[0].info.explore, true);
});

test("a turn that only thinks so far says so", () => {
  seq = 0;
  const c = fold([ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "hi" }), ev(1, { t: "turn_start" })]);
  assert.equal(codex.codexTurn(c.turns[0]).thinking, true);
  const q = chat.reduceChat(c, ev(1, { t: "question", qid: "q1", prompt: "Which?", options: ["a", "b"] }));
  assert.equal(codex.codexTurn(q.turns[0]).thinking, false, "a question waits for the user");
});

test("the plan's step and the runtime check", () => {
  assert.deepEqual(plain(codex.planStep([{ content: "a", status: "completed" }, { content: "b", status: "in_progress" }, { content: "c", status: "pending" }])),
    { step: 2, total: 3, done: 1, current: { content: "b", status: "in_progress" } });
  assert.equal(codex.isCodexRuntime({ id: "x", name: "Mine", protocol: "codex-app-server" }), true);
  assert.equal(codex.isCodexRuntime({ id: "codex", name: "Codex" }), true);
  assert.equal(codex.isCodexRuntime({ id: "claude", name: "Claude Code", protocol: "claude-stream-json" }), false);
  assert.equal(codex.relPath("/w/src/a.ts", "/w"), "src/a.ts");
  assert.equal(codex.relPath("C:\\w\\src\\a.ts", "C:\\w"), "src/a.ts");
  assert.equal(codex.relPath("/elsewhere/a.ts", "/w"), "/elsewhere/a.ts");
});

test("phase and exit codes fold into the chat state", () => {
  seq = 0;
  const c = fold([
    ev(1, { t: "message", id: "m", text: "a", delta: true, phase: "commentary" }),
    ev(1, { t: "message", id: "m", text: "b", delta: true }),
    ev(1, { t: "tool_call", call_id: "k", title: "Shell x", kind: "execute", status: "in_progress" }),
    ev(1, { t: "tool_update", call_id: "k", status: "failed", exit_code: 2, duration_ms: 50 }),
  ]);
  const [m, k] = c.turns[0].blocks;
  assert.equal(m.text, "ab");
  assert.equal(m.phase, "commentary");
  assert.equal(k.exitCode, 2);
  assert.equal(k.durationMs, 50);
});
