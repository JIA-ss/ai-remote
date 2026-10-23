// How a Codex conversation is laid out (apps/desktop/src/lib/codex.ts): commands read as what they do,
// diffs with line numbers, and a turn's work folded into units, its answer and its changes. The real
// modules are transpiled; event shapes follow the host's chat/event.rs and adapters/codex.rs.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
    vm.runInNewContext(compile(name), { exports, require, JSON, Set, Map, WeakMap, Math, Number, String, Array, Object, RegExp, Intl }, { filename: `${name}.ts` });
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
  assert.deepEqual(plain(v.units.map((u) => u.kind)), ["message", "group"], "reasoning is not a step");
  const g = v.units[1];
  assert.equal(g.live, true);
  assert.deepEqual(plain(g.acts.map((a) => a.kind)), ["exec", "exec", "patch", "exec"], "nor does it split the group");
  assert.equal(v.title, "day 31 is wrong", "its latest title is what the agent is at");
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
  // Worked until the answer started, not until the turn ended; held still whatever the time now.
  const t1 = done.turns[0];
  const answerAt = t1.blocks.find((b) => b.kind === "text" && b.id === "m2").startedTs;
  assert.equal(codex.workedMs(t1, 0), answerAt - t1.startedTs);
  assert.equal(codex.workedMs(fold(events).turns[0], 9e12), answerAt - t1.startedTs, "frozen once the answer streams");
  assert.equal(codex.workedMs(fold(codexTurn()).turns[0], t1.startedTs + 5000), 5000, "counting while it works");
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

test("the fallback reading never shows a write, a delete or a hand-off as a read", () => {
  const unknown = [{ type: "unknown" }];
  for (const c of [
    "ls build | xargs rm -rf", "cat a.txt > b.txt", "ls > out.txt", "cat install.sh | bash", "find . -name '*.tmp' -delete",
    "find . -name x -exec rm {} +", "sed -n -i '1p' x", "cat $(rm -rf x)", "echo hi > f && cat f", "grep -l foo -r . | xargs sed -i 's/a/b/'",
    "rg foo 2>/dev/null", "cat a | tee b", "sleep 1 &", "fd -x rm",
  ]) {
    assert.deepEqual(plain(codex.parseCommand(c)), unknown, c);
  }
  // Pipes into filters stay reads and searches.
  assert.deepEqual(plain(codex.parseCommand("rg --files src | sort | head -20")), [{ type: "list_files", path: "src" }]);
  assert.deepEqual(plain(codex.parseCommand("cat a.log | grep ERROR | wc -l")), [{ type: "read", path: "a.log", name: "a.log" }]);
  // A flag's value is not the file.
  assert.deepEqual(plain(codex.parseCommand("Get-Content -Path src\\app.ts -TotalCount 80")), [{ type: "read", path: "src\\app.ts", name: "app.ts" }]);
  assert.deepEqual(plain(codex.parseCommand("Get-Content src\\app.ts -Tail 20")), [{ type: "read", path: "src\\app.ts", name: "app.ts" }]);
  assert.deepEqual(plain(codex.parseCommand("rg -e foo -e bar src")), [{ type: "search", query: "foo", path: "src" }]);
  assert.deepEqual(plain(codex.parseCommand("Select-String -Pattern TODO -Path *.ts")), [{ type: "search", query: "TODO", path: "*.ts" }]);
});

test("an added file's content is content even with lines that look like hunk headers", () => {
  const md = codex.parseUnifiedDiff("# Notes\n@@ not a hunk\n- a list item\n", "add");
  assert.equal(md.add, 3);
  assert.equal(md.del, 0);
  assert.ok(md.lines.every((l) => l.kind === "add"));
  const del = codex.parseUnifiedDiff("@@ x\nold\n", "delete");
  assert.deepEqual([del.add, del.del], [0, 2]);
});

const tool = (over) => ({ kind: "tool", callId: "x", title: "edit", tool: "edit", status: "completed", locations: [], truncated: false, startedTs: 0, endedTs: 1, content: [], ...over });

test("a turn's changes: Codex's own diff when it came, else edits merged with sensible kinds", () => {
  const files = (list) => ({ files: list });
  const turn = (blocks, diff) => ({ n: 1, user: null, blocks, status: "completed", startedTs: 0, finalText: null, durationMs: 1, reason: null, usage: null, ...(diff ? { diff } : {}) });
  const del = tool({ callId: "d", input: files([{ path: "/w/a.txt", kind: "delete", diff: "x\n" }]) });
  const readd = tool({ callId: "r", input: files([{ path: "/w/a.txt", kind: "add", diff: "y\n" }]) });
  assert.equal(codex.turnChanges(turn([del, readd]))[0].kind, "update", "deleted then written again changed");
  const add = tool({ callId: "a", input: files([{ path: "/w/t.txt", kind: "add", diff: "t\n" }]) });
  const gone = tool({ callId: "g", input: files([{ path: "/w/t.txt", kind: "delete", diff: "t\n" }]) });
  assert.equal(codex.turnChanges(turn([add, gone])).length, 0, "created then deleted leaves nothing");
  // Two edits of one file: summed without the turn diff, the net result with it.
  const e1 = tool({ callId: "e1", input: files([{ path: "/w/m.py", kind: "update", diff: "@@ -1,2 +1,2 @@\n a\n-b\n+B\n" }]) });
  const e2 = tool({ callId: "e2", input: files([{ path: "/w/m.py", kind: "update", diff: "@@ -1,2 +1,2 @@\n a\n-B\n+b2\n" }]) });
  const summed = codex.turnChanges(turn([e1, e2]))[0];
  assert.deepEqual([summed.add, summed.del], [2, 2]);
  const net = "diff --git a/m.py b/m.py\nindex 1..2 100644\n--- a/m.py\n+++ b/m.py\n@@ -1,2 +1,2 @@\n a\n-b\n+b2\n"
    + "diff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+one\n+two\n";
  const fromDiff = codex.turnChanges(turn([e1, e2], { text: net, truncated: false }), "/w");
  assert.deepEqual(plain(fromDiff.map((f) => [f.path, f.kind, f.add, f.del])), [["/w/m.py", "update", 1, 1], ["/w/new.txt", "add", 2, 0]]);
  // A cut turn diff is not trusted.
  assert.equal(codex.turnChanges(turn([e1, e2], { text: net, truncated: true }))[0].add, 2);
});

test("a host past its diff budget: the file falls back to the call's content", () => {
  const x = tool({ input: { files: [{ path: "/w/big.txt", kind: "add", diff: "", truncated: true }] },
    content: [{ type: "diff", path: "/w/big.txt", old: null, new: "a\nb\n" }] });
  const [f] = codex.fileChanges(x);
  assert.equal(f.add, 2);
  assert.equal(f.truncated, true);
  assert.equal(codex.filePatch(f, "/w"), null, "no patch from a cut diff");
});

test("copied patches apply with git", (t) => {
  let git = true;
  try { execFileSync("git", ["--version"], { stdio: "ignore" }); } catch { git = false; }
  if (!git) return t.skip("no git");
  const dir = mkdtempSync(join(tmpdir(), "cdx-patch-"));
  try {
    execFileSync("git", ["init", "-q"], { cwd: dir });
    // Git for Windows turns LF into CRLF on checkout by default; the patch is about content.
    execFileSync("git", ["config", "core.autocrlf", "false"], { cwd: dir });
    mkdirSync(join(dir, "src"));
    writeFileSync(join(dir, "src", "m.py"), "a\nb\nc\n");
    writeFileSync(join(dir, "old.txt"), "bye\n");
    const files = [
      ...codex.fileChanges(tool({ callId: "1", input: { files: [{ path: join(dir, "src", "m.py"), kind: "update", diff: "@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n" }] } })),
      ...codex.fileChanges(tool({ callId: "2", input: { files: [{ path: join(dir, "new.md"), kind: "add", diff: "# New\n@@ text\n" }] } })),
      ...codex.fileChanges(tool({ callId: "3", input: { files: [{ path: join(dir, "old.txt"), kind: "delete", diff: "bye\n" }] } })),
    ];
    const patch = files.map((f) => codex.filePatch(f, dir)).join("");
    writeFileSync(join(dir, "p.diff"), patch);
    execFileSync("git", ["apply", "--check", "p.diff"], { cwd: dir });
    execFileSync("git", ["apply", "p.diff"], { cwd: dir });
    assert.equal(readFileSync(join(dir, "src", "m.py"), "utf8"), "a\nB\nc\n");
    assert.equal(readFileSync(join(dir, "new.md"), "utf8"), "# New\n@@ text\n");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
  // An older host's diffs have no line numbers: nothing to copy as a patch.
  const old = codex.fileChanges(tool({ content: [{ type: "diff", path: "a.ts", old: "1\n", new: "2\n" }] }))[0];
  assert.equal(codex.filePatch(old, null), null);
});

test("a command closed by the end of its turn is not a success; failed edits still have words", () => {
  seq = 0;
  const c = fold([
    ev(1, { t: "turn_start" }),
    ev(1, { t: "tool_call", call_id: "s", title: "Shell npm run dev", kind: "execute", status: "in_progress", input: { command: "npm run dev", actions: [{ type: "unknown" }] } }),
    ev(1, { t: "tool_call", call_id: "p", title: "edit a", kind: "edit", status: "in_progress", input: { files: [{ path: "/w/a", kind: "update", diff: "@@ -1 +1 @@\n-x\n+y\n" }] } }),
    ev(1, { t: "tool_update", call_id: "p", status: "failed" }),
    ev(1, { t: "turn_end", status: "completed", duration_ms: 10 }),
  ]);
  const t1 = c.turns[0];
  const [run, edit] = t1.blocks;
  assert.equal(run.endedByTurn, true);
  assert.equal(codex.execInfo(run).exitCode, null);
  assert.deepEqual(plain(codex.groupSummary([{ kind: "patch", key: "p", tool: edit, files: codex.fileChanges(edit) }]).parts), [{ part: "failed", n: 1 }]);
});

test("while it runs: a sub-agent is work, reasoning after the last call stays in its group, and only a completed turn has a guessed answer", () => {
  seq = 0;
  const agentRun = fold([
    ev(1, { t: "turn_start" }),
    ev(1, { t: "tool_call", call_id: "a", title: "agent spawn", kind: "think", status: "in_progress" }),
  ]);
  assert.equal(codex.codexTurn(agentRun.turns[0]).thinking, false);
  seq = 0;
  const base = [
    ev(1, { t: "turn_start" }),
    ev(1, { t: "tool_call", call_id: "c1", title: "Shell ls", kind: "execute", status: "completed", input: { command: "ls", actions: [{ type: "listFiles", path: null }] } }),
    ev(1, { t: "thought", id: "r", text: "**Next**", delta: true }),
  ];
  const live = codex.codexTurn(fold(base).turns[0]);
  assert.deepEqual(plain(live.units.map((u) => u.kind)), ["group"], "the reasoning joins the group while the turn runs");
  assert.equal(live.units[0].live, true);
  seq = 0;
  const failed = fold([
    ev(1, { t: "turn_start" }),
    ev(1, { t: "message", id: "m", text: "Trying the build.", delta: false }),
    ev(1, { t: "turn_end", status: "failed", reason: "boom", duration_ms: 5 }),
  ]);
  assert.equal(codex.codexTurn(failed.turns[0]).answer, null, "a failed turn's last remark is not its answer");
});

test("turn_diff and the turn-closed flag fold into the chat state", () => {
  seq = 0;
  const c = fold([ev(1, { t: "turn_start" }), ev(1, { t: "turn_diff", diff: "diff --git a/x b/x\n", truncated: true })]);
  assert.deepEqual(plain(c.turns[0].diff), { text: "diff --git a/x b/x\n", truncated: true });
});

test("durations: a command's run time keeps every unit from its largest; the turn's leaves out zeros", () => {
  assert.equal(codex.durationA(999, "en"), null);
  assert.equal(codex.durationA(null, "en"), null);
  assert.equal(codex.durationA(12_000, "en"), "12s");
  assert.equal(codex.durationA(60_000, "en"), "1m 0s");
  assert.equal(codex.durationA(3_603_000, "en"), "1h 0m 3s");
  assert.equal(codex.durationA(90_061_000, "en"), "1d 1h 1m 1s");
  assert.equal(codex.durationB(500, "en"), "0s");
  assert.equal(codex.durationB(60_000, "en"), "1m");
  assert.equal(codex.durationB(3_605_000, "en"), "1h 5s");
  assert.equal(codex.durationB(83_000, "zh-CN"), "1分钟 23秒");
  assert.equal(codex.durationA(12_000, "zh-CN"), "12秒");
});

test("the plan's step: the one in progress, else the first not done, else the last", () => {
  const st = (...xs) => codex.planStep(xs.map((status, i) => ({ content: String(i), status }))).step;
  assert.equal(st("completed", "pending", "completed"), 2);
  assert.equal(st("completed", "completed", "in_progress"), 3);
  assert.equal(st("completed", "completed"), 2);
  assert.equal(st("pending", "pending"), 1);
});

test("reasoning titles: the summary's last line, bold or as plain text, comments left out", () => {
  assert.equal(codex.reasoningTitle("**Reading the code**\n\nThe error is in export.py."), "The error is in export.py.");
  assert.equal(codex.reasoningTitle("Some text\n\n**Writing the fix**\n"), "Writing the fix");
  assert.equal(codex.reasoningTitle("**Planning**\n<!-- hidden -->"), "Planning");
  assert.equal(codex.reasoningTitle("Use `calendar.monthrange` for *every* month"), "Use calendar.monthrange for every month");
  assert.equal(codex.reasoningTitle("  \n "), null);
});

test("a compaction alone is not worth folding", () => {
  seq = 0;
  const c = fold([
    ev(1, { t: "turn_start" }),
    ev(1, { t: "notice", level: "info", code: "compacted", text: "Codex summarized the conversation" }),
    ev(1, { t: "message", id: "a", text: "Still here.", delta: false, phase: "final_answer" }),
    ev(1, { t: "turn_end", status: "completed", duration_ms: 10 }),
  ]);
  const v = codex.codexTurn(c.turns[0]);
  assert.equal(v.answer.block.id, "a");
  assert.equal(v.foldable, false);
});

test("questions: the latest request waits in its card, then shows as one 'Asked' line with its answers", () => {
  seq = 0;
  const q = (qid, request, extra = {}) => ev(1, { t: "question", qid, prompt: `${qid}?`, options: ["Yes (Recommended)", "No"], request, ...extra });
  let c = fold([
    ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "go" }),
    ev(1, { t: "turn_start" }),
    q("scope", "8", { header: "Scope", descriptions: ["Same bug", ""], other: true, blocking: true }),
    q("tests", "8"),
  ]);
  const p = codex.pendingRequest(c);
  assert.deepEqual(plain([p.request, p.questions.map((x) => x.qid)]), ["8", ["scope", "tests"]]);
  assert.deepEqual(plain([p.questions[0].header, p.questions[0].descriptions, p.questions[0].other, p.questions[0].blocking]), ["Scope", ["Same bug", ""], true, true]);
  let v = codex.codexTurn(c.turns[0]);
  assert.equal(v.units.length, 0, "a waiting question is not in the transcript");
  assert.equal(v.asking, true);
  assert.equal(v.thinking, false);
  // A second request asked meanwhile is the one shown.
  c = chat.reduceChat(c, q("name", "9"));
  assert.equal(codex.pendingRequest(c).request, "9");
  // The answers come back from the host (the first request's second question skipped).
  c = chat.reduceChat(c, ev(1, { t: "question_done", request: "8", answers: { scope: ["Yes (Recommended)"] }, how: "answered" }));
  c = chat.reduceChat(c, ev(1, { t: "question_done", request: "9", how: "dismissed" }));
  assert.equal(codex.pendingRequest(c), null);
  v = codex.codexTurn(c.turns[0]);
  assert.deepEqual(plain(v.units.map((u) => [u.kind, u.questions.map((x) => [x.qid, x.answers])])),
    [["asked", [["scope", ["Yes (Recommended)"]], ["tests", []]]], ["asked", [["name", []]]]]);
  assert.equal(c.turns[0].blocks.find((b) => b.qid === "name").how, "dismissed");
  // An older host names no request: the turn's open questions are one, answered one by one.
  seq = 0;
  const old = fold([ev(1, { t: "turn_start" }), ev(1, { t: "question", qid: "q1", prompt: "A?" }), ev(1, { t: "question", qid: "q2", prompt: "B?" })]);
  assert.deepEqual(plain(codex.pendingRequest(old).questions.map((x) => x.qid)), ["q1", "q2"]);
  const answered = chat.markAnswered(chat.markAnswered(old, "q1", "a"), "q2", "");
  assert.equal(codex.pendingRequest(answered), null);
  const ended = chat.reduceChat(answered, ev(1, { t: "turn_end", status: "completed", duration_ms: 1 }));
  assert.deepEqual(plain(codex.codexTurn(ended.turns[0]).units.map((u) => u.questions.length)), [2]);
});

test("an answer marks only its own request's question; the same id in an earlier turn keeps its answer", () => {
  seq = 0;
  let c = fold([
    ev(1, { t: "turn_start" }), ev(1, { t: "question", qid: "scope", prompt: "?", request: "1" }),
    ev(1, { t: "question_done", request: "1", answers: { scope: ["first"] }, how: "answered" }),
    ev(1, { t: "turn_end", status: "completed", duration_ms: 1 }),
    ev(2, { t: "turn_start" }), ev(2, { t: "question", qid: "scope", prompt: "?", request: "2" }),
  ]);
  c = chat.markAnswered(c, "scope", "second", "2");
  assert.equal(c.turns[0].blocks[0].answers[0], "first");
  assert.equal(c.turns[1].blocks[0].answer, "second");
});

test("an option's (Recommended) mark", () => {
  assert.deepEqual(plain(codex.splitRecommended("Fix it too (Recommended)")), { label: "Fix it too", recommended: true });
  assert.deepEqual(plain(codex.splitRecommended("Only export.py")), { label: "Only export.py", recommended: false });
});

test("a message remembers when it started", () => {
  seq = 0;
  const c = fold([ev(1, { t: "turn_start" }), ev(1, { t: "message", id: "m", text: "a", delta: true }), ev(1, { t: "message", id: "m", text: "b", delta: true })]);
  const m = c.turns[0].blocks[0];
  assert.equal(m.text, "ab");
  assert.equal(m.startedTs, 1200);
});

test("terminal colors: SGR runs, 256 and true color, carriage returns and backspaces", () => {
  assert.deepEqual(plain(codex.parseAnsi("\x1b[31mred\x1b[0m ok")), [{ text: "red", fg: "c1" }, { text: " ok" }]);
  assert.deepEqual(plain(codex.parseAnsi("abc\rxy")), [{ text: "xyc" }]);
  assert.deepEqual(plain(codex.parseAnsi("ab\b")), [{ text: "a" }]);
  assert.deepEqual(plain(codex.parseAnsi("10%\r100%\nnext")), [{ text: "100%\nnext" }]);
  assert.deepEqual(plain(codex.parseAnsi("\x1b[38;5;196mX\x1b[48;2;1;2;3mY")), [{ text: "X", fg: "rgb(255 0 0)" }, { text: "Y", fg: "rgb(255 0 0)", bg: "rgb(1 2 3)" }]);
  assert.deepEqual(plain(codex.parseAnsi("\x1b[1;4mB\x1b[22mU\x1b[0m\x1b[92mg")), [{ text: "B", bold: true, underline: true }, { text: "U", underline: true }, { text: "g", fg: "c2" }]);
  assert.deepEqual(plain(codex.parseAnsi("\x1b]0;title\x07plain\x1b[2K")), [{ text: "plain" }], "other escapes are dropped");
});

test("web searches: the query with its sites; a curl of a page off the internet counts as one", () => {
  assert.equal(codex.webDetail("foo site:www.a.com OR site:b.com"), "foo | a.com · b.com");
  assert.equal(codex.webDetail("python calendar monthrange"), "python calendar monthrange");
  const info = (command) => ({ command, cwd: null, actions: [{ type: "unknown" }], explore: false, output: "", exitCode: 0, durationMs: null });
  assert.equal(codex.isWebFetch(info("curl -sL https://example.com/docs")), true);
  assert.equal(codex.isWebFetch(info("curl http://localhost:3000/api")), false);
  assert.equal(codex.isWebFetch(info("curl -X POST https://example.com")), false);
  assert.equal(codex.isWebFetch(info("wget https://example.com")), false);
});

test("a group's sentence names tools, edits, reads, commands, then the web", () => {
  const t = (over) => tool({ status: "completed", ...over });
  const acts = [
    { kind: "web", key: "w", tool: t({ tool: "fetch", title: "search x" }), query: "x" },
    { kind: "exec", key: "e", tool: t({ tool: "execute" }), info: { command: "npm test", actions: [{ type: "unknown" }], explore: false, exitCode: 0 } },
    { kind: "exec", key: "c", tool: t({ tool: "execute" }), info: { command: "curl https://example.com", actions: [{ type: "unknown" }], explore: false, exitCode: 0 } },
    { kind: "patch", key: "p", tool: t({}), files: [{ path: "/a", kind: "update", add: 1, del: 0, lines: [] }] },
    { kind: "tool", key: "m", tool: t({ tool: "other", title: "mcp" }) },
  ];
  assert.deepEqual(plain(codex.groupSummary(acts).parts.map((p) => [p.part, p.n])), [["tools", 1], ["edited", 1], ["ran", 1], ["web", 2]]);
  assert.deepEqual(plain(codex.groupSummary([]).parts), []);
});

test("message times: today the time, this week the weekday, else the date", () => {
  const now = new Date(2026, 9, 23, 15, 0).getTime();
  assert.match(codex.formatMsgTime(new Date(2026, 9, 23, 9, 5).getTime(), now, "en"), /^9:05\s?AM$/);
  assert.match(codex.formatMsgTime(new Date(2026, 9, 21, 9, 5).getTime(), now, "en"), /^Wednesday 9:05\s?AM$/);
  assert.match(codex.formatMsgTime(new Date(2026, 8, 30, 14, 2).getTime(), now, "en"), /^Sep 30 2:02\s?PM$/);
  assert.equal(codex.formatMsgTime(new Date(2026, 9, 23, 14, 2).getTime(), now, "zh-CN"), "14:02");
});

test("a request's end touches only the turn that asked it, even when a later process reuses its id", () => {
  seq = 0;
  let c = fold([
    ev(1, { t: "turn_start" }), ev(1, { t: "question", qid: "scope", prompt: "?", request: "0" }),
    ev(1, { t: "question_done", request: "0", answers: { scope: ["Yes"] }, how: "answered" }),
    ev(1, { t: "turn_end", status: "completed", duration_ms: 1 }),
    ev(2, { t: "turn_start" }), ev(2, { t: "question", qid: "scope", prompt: "?", request: "0", expires_in_ms: 120000 }),
  ]);
  const second = c.turns[1].blocks[0];
  assert.equal(second.expiresAt, second.ts ?? (1000 + 6 * 100) + 120000, "the deadline is the event's time and the wait");
  c = chat.markAnswered(c, "scope", "All", "0", 2);
  c = chat.reduceChat(c, ev(2, { t: "question_done", request: "0", how: "dismissed" }));
  assert.deepEqual(plain([c.turns[0].blocks[0].answers, c.turns[0].blocks[0].how]), [["Yes"], "answered"]);
  assert.equal(c.turns[1].blocks[0].how, "dismissed");
  // An answer never rewrites a question already answered.
  const again = chat.markAnswered(c, "scope", "Other");
  assert.equal(again.turns[0].blocks[0].answers[0], "Yes");
});

test("terminal lines: CRLF keeps colors, erase and backspace work around escapes, a cut escape waits", () => {
  assert.deepEqual(plain(codex.parseAnsi("\x1b[1;31mFAIL\x1b[0m test\r\n\x1b[32mPASS\x1b[0m\r\n")),
    [{ text: "FAIL", bold: true, fg: "c1" }, { text: " test\n" }, { text: "PASS", fg: "c2" }, { text: "\n" }]);
  assert.deepEqual(plain(codex.parseAnsi("Downloading 50%\r\x1b[2KDone\n")), [{ text: "Done\n" }]);
  assert.deepEqual(plain(codex.parseAnsi("x 50%\x1b[2K\rDone")), [{ text: "Done" }]);
  assert.deepEqual(plain(codex.parseAnsi("Working \x1b[1m|\x1b[0m\b\x1b[1m/\x1b[0m\bdone")), [{ text: "Working done" }]);
  assert.deepEqual(plain(codex.parseAnsi("a\x1b[31m\bb")), [{ text: "b", fg: "c1" }]);
  assert.deepEqual(plain(codex.parseAnsi("abc\x1b[0;3")), [{ text: "abc" }]);
  assert.deepEqual(plain(codex.parseAnsi("abc\x1b]8;;https://x.example")), [{ text: "abc" }]);
});

test("only a plain curl that reads a page counts as a web search", () => {
  const info = (command) => ({ command, cwd: null, actions: [{ type: "unknown" }], explore: false, output: "", exitCode: 0, durationMs: null });
  for (const c of ["curl -fsSL https://get.example.com/install.sh | sh", "curl -XDELETE https://api.example.com/items/3", "curl https://api.example.com --request=DELETE",
    "curl --json '{}' https://api.example.com", "curl -sO https://example.com/big.iso", "curl -s https://example.com && rm -rf build", "curl https://example.com > page.html"]) {
    assert.equal(codex.isWebFetch(info(c)), false, c);
  }
  assert.equal(codex.isWebFetch(info("curl -s -H 'Accept: text/html' https://example.com")), true);
});

test("a file created and then edited in a turn without its diff: counted as created, no patch", () => {
  const add = tool({ callId: "a", input: { files: [{ path: "/w/n.txt", kind: "add", diff: "a\nb\nc\n" }] } });
  const upd = tool({ callId: "u", input: { files: [{ path: "/w/n.txt", kind: "update", diff: "@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n" }] } });
  const turn = { n: 1, user: null, blocks: [add, upd], status: "completed", startedTs: 0, finalText: null, durationMs: 1, reason: null, usage: null };
  const [f] = codex.turnChanges(turn);
  assert.deepEqual(plain([f.kind, f.add, f.del, f.merged]), ["add", 3, 0, true]);
  assert.equal(codex.filePatch(f, "/w"), null);
});

test("a turn stopped while answering says how long it ran in all", () => {
  seq = 0;
  const c = fold([
    ev(1, { t: "turn_start" }),
    ev(1, { t: "tool_call", call_id: "c", title: "Shell ls", kind: "execute", status: "completed", input: { command: "ls", actions: [{ type: "listFiles", path: null }] } }),
    ev(1, { t: "message", id: "a", text: "Here", delta: true, phase: "final_answer" }),
    ev(1, { t: "turn_end", status: "interrupted", duration_ms: 62000 }),
  ]);
  assert.equal(codex.workedMs(c.turns[0], 0), 62000);
});
