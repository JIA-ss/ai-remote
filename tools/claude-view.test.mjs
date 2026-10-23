// How the Claude view lays out a conversation (apps/desktop/src/lib/claude.ts): a group of tool calls
// summed up in one sentence, how one call reads, the status line's anti-flicker timers, which keys
// answer an approval card, a question's pages and answers, numbers, diffs and the task list window.
// The real modules are transpiled; event shapes follow docs/agent-workspace.zh-CN.md §1.
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
    vm.runInNewContext(compile(name), { exports, require, JSON, Set, Map, Math, Number, String, Array, Object, RegExp, URL }, { filename: `${name}.ts` });
    return exports;
  };
  return { chat: run("chat"), cl: run("claude") };
}

const { chat, cl } = load();
const plain = (v) => JSON.parse(JSON.stringify(v));
let seq = 0;
const ev = (turn, e) => ({ seq: ++seq, ts: 1000 + seq * 100, turn, ...e });
const fold = (events) => events.reduce(chat.reduceChat, chat.emptyChat());
let n = 0;
const tool = (title, kind, extra = {}) => ({ kind: "tool", callId: `k${++n}`, title, tool: kind, status: "completed", input: undefined, locations: [],
  content: [], truncated: false, startedTs: 0, endedTs: 1, ...extra });
const read = (path, extra) => tool(`Read ${path}`, "read", { locations: [path], ...extra });
const bash = (command, extra) => tool(`Bash ${command}`, "execute", { input: { command }, ...extra });
const edit = (path, old, neu, extra) => tool(`Edit ${path}`, "edit", { locations: [path], content: [{ type: "diff", path, old, new: neu }], ...extra });
const write = (path, text, extra) => tool(`Write ${path}`, "edit", { locations: [path], content: [{ type: "diff", path, old: null, new: text }], ...extra });

// ---- Grouping and the group's sentence -------------------------------------------------------

test("prose breaks the work into groups; reasoning and calls in a row are one group; a sub-agent is its own row", () => {
  seq = 0;
  const c = fold([
    ev(0, { t: "user", input_id: "u1", by: { device: "d" }, text: "fix it" }),
    ev(1, { t: "turn_start" }),
    ev(1, { t: "thought", id: "r1", text: "look first", delta: false }),
    ev(1, { t: "tool_call", call_id: "a", title: "Read /w/a.py", kind: "read", status: "completed", locations: ["/w/a.py"] }),
    ev(1, { t: "plan", entries: [{ content: "x", status: "in_progress" }] }),
    ev(1, { t: "tool_call", call_id: "b", title: "Bash pytest", kind: "execute", status: "completed", input: { command: "pytest" } }),
    ev(1, { t: "message", id: "m1", text: "Found it.", delta: false }),
    ev(1, { t: "tool_call", call_id: "ag", title: "Agent Find the code", kind: "think", status: "completed" }),
    ev(1, { t: "tool_call", call_id: "c1", parent: "ag", title: "Grep foo", kind: "search", status: "completed" }),
    ev(1, { t: "tool_call", call_id: "e", title: "Edit /w/a.py", kind: "edit", status: "completed", content: [{ type: "diff", path: "/w/a.py", old: "a", new: "b" }] }),
    ev(1, { t: "message", id: "m2", text: "Done", delta: false }),
    ev(1, { t: "turn_end", status: "completed", duration_ms: 10 }),
  ]);
  const rows = cl.claudeRows(c.turns[0]);
  assert.deepEqual(plain(rows.map((r) => r.kind)), ["user", "group", "text", "agent", "group", "text"]);
  assert.deepEqual(plain(rows[1].members.map((m) => m.kind)), ["thought", "tool", "tool"], "the to-do list does not break the group");
  assert.deepEqual(plain(rows[3].children.map((x) => x.callId)), ["c1"]);
  assert.equal(cl.singleCall(rows[4]).callId, "e", "a finished group of one call is that call's row");
  assert.equal(cl.singleCall(rows[1]), null);
});

test("while the turn runs its last group is live and its last text streams", () => {
  seq = 0;
  const c = fold([
    ev(0, { t: "user", input_id: "u1", by: { device: "d" }, text: "go" }),
    ev(1, { t: "tool_call", call_id: "a", title: "Bash sleep 9", kind: "execute", status: "in_progress" }),
    ev(1, { t: "message", id: "m", text: "Wait", delta: true }),
  ]);
  const rows = cl.claudeRows(c.turns[0]);
  assert.equal(rows.find((r) => r.kind === "group").live, true, "a group with a running call is live");
  assert.equal(rows.at(-1).streaming, true);
  assert.equal(cl.singleCall(rows.find((r) => r.kind === "group")), null, "a live group keeps its frame");
});

test("the sentence: kinds in the order they came, files counted once, failures noted", () => {
  const tools = [read("/w/a.py"), read("/w/a.py"), read("/w/b.py"), bash("pytest", { status: "failed" }), bash("ls"), edit("/w/a.py", "x", "y")];
  const s = cl.groupSentence(tools);
  assert.deepEqual(plain(s.phrases.map((p) => [p.cat, p.n, p.failed, p.error])), [["read", 2, 0, false], ["command", 2, 1, false], ["edit", 1, 0, false]]);
  assert.equal(s.more, null);
  assert.equal(s.file, null);
  // Every call of a kind failed: that phrase says so.
  const bad = cl.groupSentence([bash("a", { status: "failed" }), bash("b", { status: "failed" })]);
  assert.equal(bad.phrases[0].error, true);
});

test("five kinds or more: three are named and the rest are 'N more'; tools without a word come last", () => {
  const tools = [tool("mcp__gh__get_issue", "other"), read("/a"), bash("ls"), tool("Grep x", "search"), tool("WebFetch https://x.dev", "fetch"),
    tool("WebSearch rust", "fetch"), tool("WebSearch go", "fetch", { status: "failed" })];
  const s = cl.groupSentence(tools);
  assert.deepEqual(plain(s.phrases.map((p) => p.cat)), ["read", "command", "search"]);
  assert.deepEqual(plain(s.more), { n: 4, failed: 1, blocked: 0 }, "fetch 1 + web search 2 + the uncategorized 1");
  // Four kinds are all named.
  assert.equal(cl.groupSentence(tools.slice(0, 4)).phrases.length, 4);
  assert.equal(cl.groupSentence(tools.slice(0, 4)).phrases.at(-1).cat, "tool");
});

test("all the work on one file reads as one sentence; it opens the diff once an edit went through", () => {
  const s = cl.groupSentence([read("/w/export.py"), edit("/w/export.py", "a", "b"), read("/w/export.py")]);
  assert.deepEqual(plain(s.file), { path: "/w/export.py", cats: ["read", "edit"], preferDiff: true });
  assert.equal(cl.groupSentence([read("/w/a"), read("/w/b")]).file, null);
  assert.equal(cl.groupSentence([read("/w/a"), bash("ls")]).file, null);
  assert.equal(cl.groupSentence([read("/w/a")]).file.preferDiff, false);
});

test("a Write over an existing file counts as an edit; a new one as created", () => {
  assert.equal(cl.toolCat(write("/w/new.md", "# hi")), "create");
  const over = tool("Write /w/a.md", "edit", { locations: ["/w/a.md"], content: [{ type: "diff", path: "/w/a.md", old: null, new: "b",
    hunks: [{ old_start: 1, old_lines: 1, new_start: 1, new_lines: 1, lines: ["-a", "+b"] }] }] });
  assert.equal(cl.toolCat(over), "edit");
  // An agent without Claude's names is read by kind.
  assert.equal(cl.toolCat(tool("Reading file notes.txt", "read")), "read");
  assert.equal(cl.toolName(tool("Reading file notes.txt", "read")), "");
  assert.equal(cl.toolName(tool("SomeNewTool", "other")), "SomeNewTool");
});

test("blocked and denied calls: by the call id the host names, or by tool and input", () => {
  seq = 0;
  let c = fold([
    ev(0, { t: "user", input_id: "u1", by: { device: "d" }, text: "go" }),
    ev(1, { t: "tool_call", call_id: "a", title: "Bash rm -rf out", kind: "execute", status: "in_progress", input: { command: "rm -rf out" } }),
    ev(1, { t: "tool_call", call_id: "b", title: "Bash ls", kind: "execute", status: "in_progress", input: { command: "ls" } }),
    ev(1, { t: "permission", rid: "p1", tool: "Bash", input: { command: "rm -rf out" } }),
  ]);
  let map = cl.requestsByCall(c.turns[0], c.requests);
  assert.deepEqual(plain([...map.keys()]), ["a"], "matched by input, not just the newest Bash");
  const a = c.turns[0].blocks.find((b) => b.callId === "a");
  assert.equal(cl.toolState(a, map.get("a")), "blocked");
  c = chat.reduceChat(c, ev(1, { t: "resolved", id: "p1", outcome: "denied" }));
  map = cl.requestsByCall(c.turns[0], c.requests);
  assert.equal(cl.toolState(a, map.get("a")), "denied");
  const s = cl.groupSentence(c.turns[0].blocks.filter((b) => b.kind === "tool"), new Map([["a", "denied"], ["b", "busy"]]));
  assert.deepEqual(plain(s.phrases.map((p) => [p.n, p.denied])), [[2, 1]]);
  // Without a permission: Claude's fixed words for a refusal still read as denied.
  assert.equal(cl.toolState(bash("x", { status: "failed", content: [{ type: "text", text: "The user doesn't want to proceed with this tool use." }] })), "denied");
  assert.equal(cl.toolState(bash("x", { status: "failed", stopped: true })), "stopped");
  assert.equal(cl.toolState(bash("x", { status: "failed" })), "failed");
});

// ---- One call's words ------------------------------------------------------------------------

test("a command's own description changes tense; a command without one shows its first safe words", () => {
  const withDesc = bash("pytest -q", { input: { command: "pytest -q", description: "Run the test suite and check coverage" } });
  assert.equal(cl.toolWords(withDesc, "busy").text, "Running the test suite and checking coverage");
  assert.equal(cl.toolWords(withDesc, "done").text, "Ran the test suite and checked coverage");
  assert.equal(cl.toolWords(withDesc, "failed").failedText, "run the test suite and check coverage");
  // "and" followed by is/are/was stays.
  assert.equal(cl.describe("Check what is there and is not tracked", "done").text, "Checked what is there and is not tracked");
  assert.equal(cl.describe("Re-run the build", "busy").text, "Re-running the build");
  assert.deepEqual(plain(cl.describe("Quarterly export", "done")), { text: "Quarterly export", known: false });
  const plainCmd = cl.toolWords(bash("cd /w && python export.py --quarter Q3"), "done");
  assert.deepEqual(plain([plainCmd.verb, plainCmd.obj, plainCmd.objKind]), ["command", "python export.py…", "code"]);
  assert.equal(cl.toolWords(bash("npm test"), "busy").obj, "npm test");
});

test("command summaries stop at anything that looks like a secret, an address or an unknown program", () => {
  assert.equal(cl.commandSummary("git status"), "git status");
  assert.equal(cl.commandSummary("git log --oneline -5"), "git log…");
  assert.equal(cl.commandSummary("ls src\necho done"), "ls src…");
  assert.equal(cl.commandSummary("curl https://x.dev"), null, "curl is not in the list");
  assert.equal(cl.commandSummary("echo $(cat key)"), null);
  assert.equal(cl.commandSummary("git clone https://user:pw@host/repo"), null);
  assert.equal(cl.commandSummary("export GITHUB_TOKEN=abc && git push"), null);
  assert.equal(cl.commandSummary("npm publish --otp 123 && echo npm_abcdef"), null);
  assert.equal(cl.commandSummary("mysql -u root -psecret db"), null);
  assert.equal(cl.commandSummary("cat ~/.claude/projects/x/memory.md"), null);
  assert.equal(cl.commandSummary("cat a8f7d6e5c4b3a2f1e0d9c8b7"), "cat…", "a 20+ letter-and-digit run is a token");
  assert.equal(cl.commandSummary("git push origin main | tee log"), "git push origin main…");
  assert.equal(cl.commandSummary("FOO=1 python run.py"), null, "an assignment is not a word to show (the program is checked past it)");
  assert.equal(cl.sensitiveCommand("curl -H 'Authorization: Bearer x'"), true);
});

test("files read as their names; searches and fetches carry what they looked for; MCP tools by server", () => {
  const r = cl.toolWords(read("/w/src/export.py"), "busy");
  assert.deepEqual(plain([r.verb, r.obj, r.objKind, r.path]), ["read", "export.py", "file", "/w/src/export.py"]);
  assert.equal(cl.toolWords(write("/w/new.md", "x"), "done").verb, "create");
  assert.deepEqual(plain(cl.toolWords(tool("Grep quarter_end", "search", { input: { pattern: "quarter_end" } }), "done")).obj, "quarter_end");
  assert.equal(cl.toolWords(tool("mcp__github__get_issue", "other"), "done").obj, "github: get issue");
  const q = cl.toolWords(tool("AskUserQuestion", "other", { input: { questions: [{ question: "a" }, { question: "b" }] } }), "done");
  assert.deepEqual(plain([q.verb, q.n]), ["questions", 2]);
  assert.equal(cl.toolWords(tool("Skill", "other", { input: { skill: "tidy" } }), "done").obj, "/tidy");
});

// ---- Approval card ----------------------------------------------------------------------------

const key = (k, over = {}) => ({ key: k, meta: false, ctrl: false, shift: false, alt: false, repeat: false, composing: false, now: 10_000, focus: "composer", ...over });
const ctx = (over = {}) => ({ armedAt: 0, draft: false, session: true, buttons: 3, ...over });

test("approval keys: Esc denies, ⌘↵ allows once, ⌘⇧↵ for the session, 1–9 press buttons outside text fields", () => {
  assert.deepEqual(plain(cl.approvalKey(key("Escape"), ctx())), { act: "deny" });
  assert.deepEqual(plain(cl.approvalKey(key("Enter", { meta: true }), ctx())), { act: "once" });
  assert.deepEqual(plain(cl.approvalKey(key("Enter", { ctrl: true }), ctx())), { act: "once" }, "Ctrl on Windows");
  assert.deepEqual(plain(cl.approvalKey(key("Enter", { meta: true, shift: true }), ctx())), { act: "session" });
  assert.equal(cl.approvalKey(key("Enter", { meta: true, shift: true }), ctx({ session: false })), null);
  assert.equal(cl.approvalKey(key("Enter"), ctx()), null, "plain Enter is the composer's");
  assert.equal(cl.approvalKey(key("2"), ctx()), null, "digits typed into the composer are text");
  assert.deepEqual(plain(cl.approvalKey(key("2", { focus: "card" }), ctx())), { act: "button", index: 1 });
  assert.equal(cl.approvalKey(key("4", { focus: "other" }), ctx()), null, "no fourth button");
});

test("approval keys: 400 ms after the card appears and held keys are swallowed; a draft keeps ⌘↵", () => {
  assert.deepEqual(plain(cl.approvalKey(key("Enter", { meta: true, now: 300 }), ctx({ armedAt: 0 }))), { act: "swallow" });
  assert.deepEqual(plain(cl.approvalKey(key("Escape", { now: 399 }), ctx())), { act: "swallow" });
  assert.deepEqual(plain(cl.approvalKey(key("Escape", { now: 400 }), ctx())), { act: "deny" });
  assert.deepEqual(plain(cl.approvalKey(key("Enter", { meta: true, repeat: true }), ctx())), { act: "swallow" });
  assert.equal(cl.approvalKey(key("Enter", { meta: true }), ctx({ draft: true })), null, "⌘↵ sends the draft");
  assert.deepEqual(plain(cl.approvalKey(key("Enter", { meta: true, shift: true }), ctx({ draft: true }))), { act: "session" }, "⌘⇧↵ still answers");
  assert.equal(cl.approvalKey(key("Enter", { meta: true, composing: true }), ctx()), null);
  assert.equal(cl.approvalKey(key("Enter", { meta: true, alt: true }), ctx()), null);
  assert.equal(cl.approvalKey(key("Escape", { focus: "field" }), ctx()), null, "another field keeps its keys");
  assert.equal(cl.approvalKey(key("Escape"), ctx({ menuOpen: true })), null);
  // Click-only requests: no ⌘↵ and no digits, Esc still denies.
  assert.equal(cl.approvalKey(key("Enter", { meta: true }), ctx({ clickOnly: true })), null);
  assert.equal(cl.approvalKey(key("1", { focus: "card" }), ctx({ clickOnly: true })), null);
  assert.deepEqual(plain(cl.approvalKey(key("Escape"), ctx({ clickOnly: true }))), { act: "deny" });
});

test("an approval card says what the tool would do", () => {
  const p = (tool, input, extra = {}) => ({ kind: "permission", id: "p", turn: 1, seq: 1, ts: 0, tool, input, suggestions: [], resolved: null, ...extra });
  const b = cl.permitView(p("Bash", { command: "cargo build -p ar-host\n--release", description: "Build the host" }, { suggestions: ["session"], reason: "rule" }));
  assert.deepEqual(plain([b.action, b.object, b.command, b.description, b.session, b.reason]),
    ["run", "Build the host", "cargo build -p ar-host\n--release", "Build the host", true, "rule"]);
  assert.equal(cl.permitView(p("Bash", { command: "ls -la\nwc" })).object, "ls -la");
  const e = cl.permitView(p("Edit", { file_path: "/w/a.py", old_string: "a", new_string: "b" }));
  assert.deepEqual(plain([e.action, e.object, e.diff]), ["edit", "a.py", { path: "/w/a.py", old: "a", new: "b" }]);
  assert.equal(cl.permitView(p("WebFetch", { url: "https://docs.rs/tokio/latest" })).object, "docs.rs");
  assert.equal(cl.permitView(p("mcp__github__create_issue", {})).object, "github: create issue");
  assert.equal(cl.permitView(p("Bash", { command: "x", run_in_background: true })).background, true);
});

// ---- Question card ---------------------------------------------------------------------------

const parts = [
  { header: "Config", question: "Which config?", options: [{ label: "debug" }, { label: "release" }] },
  { question: "Which targets?", options: [{ label: "linux" }, { label: "mac" }, { label: "win" }], multi: true },
  { question: "Anything else?", options: [{ label: "no" }] },
];

test("question pages: Next needs an answer, Skip moves on, the last page submits only when all are answered", () => {
  let d = cl.qInit(3);
  assert.deepEqual(plain(cl.qNext(d, parts)), { draft: plain(d), submit: false }, "nothing picked: Next does nothing");
  d = cl.qPick(d, parts, 0, "release");
  ({ draft: d } = cl.qNext(d, parts));
  assert.equal(d.page, 1);
  d = cl.qPick(d, parts, 1, "win");
  d = cl.qPick(d, parts, 1, "linux");
  d = cl.qPick(d, parts, 1, "win");
  assert.deepEqual(plain(d.picks[1]), ["linux"], "several choices toggle");
  // Skip on the last page with nothing left open submits.
  ({ draft: d } = cl.qNext(d, parts));
  let r = cl.qSkip(d, parts);
  assert.equal(r.submit, true);
  assert.deepEqual(plain(cl.qAnswers(r.draft, parts)), { "Which config?": "release", "Which targets?": "linux", "Anything else?": "[No preference]" });
  // Skipping from the last page goes back to the first part left open.
  let e = { ...cl.qInit(3), page: 2 };
  r = cl.qSkip(e, parts);
  assert.deepEqual(plain([r.submit, r.draft.page]), [false, 0]);
});

test("'Other' is an answer of its own; empty it says so; one choice replaces it, several keep it", () => {
  let d = cl.qInit(3);
  d = cl.qOther(d, parts, 0, "staging");
  assert.equal(cl.qAnswered(d, 0), true);
  d = cl.qPick(d, parts, 0, "debug");
  assert.equal(d.other[0], null, "picking a choice of a one-choice part drops 'Other'");
  d = cl.qPick(d, parts, 1, "mac");
  d = cl.qOther(d, parts, 1, "  ");
  d = cl.qPick(d, parts, 1, "linux");
  d = cl.qPick(d, parts, 2, "no");
  assert.deepEqual(plain(cl.qAnswers(d, parts)), { "Which config?": "debug", "Which targets?": "linux, mac, [Other, no details given]", "Anything else?": "no" },
    "several in the options' order, 'Other' last");
  // An older host's question is one part made of its prompt and options.
  assert.deepEqual(plain(cl.questionParts({ kind: "question", qid: "q", prompt: "Which?", options: ["a", "b"], answered: false })),
    [{ question: "Which?", options: [{ label: "a" }, { label: "b" }] }]);
});

// ---- Status line -----------------------------------------------------------------------------

test("the status line: stopping, connecting, waiting, a running tool, thinking by the turn's seconds, a question", () => {
  seq = 0;
  const base = [ev(0, { t: "state", state: "running", queued: [] }), ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "go" }), ev(1, { t: "turn_start" })];
  const at = (events, over = {}) => cl.statusOf({ chat: fold(events), connected: true, sending: false, stopping: false, now: 1400 + 40_000, ...over });
  assert.equal(at(base).phase, "waiting");
  assert.equal(at(base, { stopping: true }).phase, "stopping");
  assert.equal(at(base, { connected: false }).phase, "connecting");
  const thinking = [...base, ev(1, { t: "thought", id: "r", text: "hmm", delta: true })];
  const st = at(thinking);
  assert.deepEqual(plain([st.phase, st.tier]), ["thinking", 2], "40 s into the turn is the third wording");
  assert.deepEqual(plain(cl.THINK_TIERS.map((s) => cl.thinkTier(s - 0.1))), [0, 1, 2, 3]);
  assert.equal(cl.thinkTier(75), 4);
  assert.equal(at([...thinking, ev(1, { t: "tool_call", call_id: "k", title: "Bash x", kind: "execute", status: "in_progress" })]).phase, "tool");
  assert.equal(at([...thinking, ev(1, { t: "message", id: "m", text: "So", delta: true })]).phase, "thought");
  assert.equal(at([...base, ev(1, { t: "permission", rid: "p", tool: "Bash" })]).phase, "asking");
  assert.equal(cl.statusOf({ chat: chat.emptyChat(), connected: true, sending: false, stopping: false, now: 0 }).on, false);
  assert.equal(cl.statusOf({ chat: chat.emptyChat(), connected: true, sending: true, stopping: false, now: 0 }).phase, "sending");
});

test("a label must hold 150 ms before it replaces another; empty and new epochs show at once", () => {
  let s = cl.debounceStep(null, "connecting", 0).state;
  assert.equal(s.shown, "connecting");
  let r = cl.debounceStep(s, "sending", 100);
  assert.deepEqual(plain([r.state.shown, r.wake]), ["connecting", 250]);
  r = cl.debounceStep(r.state, "sending", 200);
  assert.equal(r.state.shown, "connecting", "still within 150 ms of when it first came");
  r = cl.debounceStep(r.state, "sending", 250);
  assert.equal(r.state.shown, "sending");
  // Flapping back and forth never shows the flap.
  r = cl.debounceStep(r.state, "tool", 300);
  r = cl.debounceStep(r.state, "sending", 350);
  r = cl.debounceStep(r.state, "tool", 400);
  assert.equal(r.state.shown, "sending");
  assert.equal(cl.debounceStep(r.state, null, 410).state.shown, null, "becoming empty is at once");
  assert.equal(cl.debounceStep(r.state, null, 410, 0, true).state.shown, "sending", "unless the turn has said nothing yet");
  assert.equal(cl.debounceStep(cl.debounceStep(r.state, null, 410).state, "x", 411).state.shown, "x", "coming from empty is at once");
  assert.equal(cl.debounceStep(r.state, "other", 420, 1).state.shown, "other", "a new turn starts over");
});

test("what is shown stays 650 ms; a new epoch resets that", () => {
  let d = cl.dwellStep(null, "Reading a.py", 0).state;
  let r = cl.dwellStep(d, "Reading b.py", 100);
  assert.deepEqual(plain([r.state.shown, r.wake]), ["Reading a.py", 650]);
  r = cl.dwellStep(r.state, "Reading b.py", 650);
  assert.equal(r.state.shown, "Reading b.py");
  r = cl.dwellStep(r.state, "Editing a.py", 700);
  assert.equal(r.state.shown, "Reading b.py");
  assert.equal(cl.dwellStep(r.state, "Editing a.py", 701, 2).state.shown, "Editing a.py");
  assert.equal(cl.dwellStep(r.state, "Reading b.py", 702).wake, null, "the same value needs nothing");
});

// ---- Numbers, pacing, diffs, files, the task list ---------------------------------------------

test("tokens and durations as the status line says them", () => {
  assert.deepEqual(plain([950, 1000, 12_049, 999_960, 1_250_000, 3e9].map(cl.tokenCount)), ["950", "1.0k", "12.0k", "1.0M", "1.3M", "3.0B"]);
  assert.deepEqual(plain([120_500, 200_000].map(cl.tokenShort)), ["120.5k", "200k"]);
  const d = (ms) => cl.durationParts(ms).map((p) => `${p.n}${p.unit}`).join(" ");
  assert.equal(d(0), "0s");
  assert.equal(d(65_000), "1m 5s");
  assert.equal(d(3_600_000), "1h");
  assert.equal(d(90_061_000), "1d 1h 1m", "at most three units");
  assert.deepEqual(plain([74, 75, 89, 90].map(cl.ringTier)), ["normal", "warning", "warning", "critical"]);
});

test("a streamed reply comes out in word-sized steps, faster with more waiting; big jumps show at once", () => {
  const text = "The quarter end was always day thirty-one, which June does not have. ".repeat(4);
  assert.equal(cl.paceStart(text, false), 0);
  assert.equal(cl.paceStart(text, true), text.length, "text shown before is not paced again");
  assert.equal(cl.paceStart("x".repeat(401), false), 401, "a first block over 400 characters shows whole");
  let r = cl.paceNext(0, text);
  assert.ok(r.next <= 22 && text[r.next - 1] === " ", `cut after a word (${r.next})`);
  assert.equal(r.delay, 150 > (550 * 22) / text.length ? Math.max(25, (550 * 22) / text.length) : 150);
  assert.equal(cl.paceNext(0, "short").delay, 150, "little waiting: the slowest step");
  assert.equal(cl.paceNext(0, "x".repeat(2000)).delay, 25, "much waiting: the fastest");
  assert.equal(cl.paceNext(0, "x".repeat(700), 700).next, 700, "over 600 waiting and arrived at once: caught up");
  assert.deepEqual(plain(cl.paceNext(5, "short")), { next: 5, delay: null });
});

test("a diff's hunks give real line numbers and gaps; without them the diff is computed", () => {
  const hunks = [
    { old_start: 3, old_lines: 3, new_start: 3, new_lines: 4, lines: [" a", "-b", "+B", "+C", " d"] },
    { old_start: 20, old_lines: 1, new_start: 21, new_lines: 1, lines: ["-x", "+y", "\\ No newline at end of file"] },
  ];
  const rows = cl.hunkLines(hunks);
  assert.deepEqual(plain(rows.map((r) => (r.kind === "gap" ? `gap ${r.lines}` : `${r.kind} ${r.old ?? "-"} ${r.new ?? "-"} ${r.text}`))),
    ["context 3 3 a", "del 4 - b", "add - 4 B", "add - 5 C", "context 5 6 d", "gap 14", "del 20 - x", "add - 21 y"]);
  const c = { type: "diff", path: "a", old: "b", new: "B", hunks };
  assert.deepEqual(plain(cl.contentStat(c)), { add: 3, del: 2 });
  assert.equal(cl.diffLines(c).numbered, true);
  assert.equal(cl.diffLines({ type: "diff", path: "a", old: "1\n2", new: "1\n3" }).numbered, false);
  assert.equal(cl.hunksOk([{ old_start: 1, new_start: 1, old_lines: 1, new_lines: 1, lines: ["?bad"] }]), false);
});

test("a Read's output is the file's text: line numbers and the appended reminder go", () => {
  const out = "     5→import os\n     6→\n     7→def f():\n\n<system-reminder>\nnot for you\n</system-reminder>";
  assert.deepEqual(plain(cl.readText(out)), { text: "import os\n\ndef f():", first: 5 });
  assert.deepEqual(plain(cl.readText("plain\ntext")), { text: "plain\ntext", first: null });
});

test("the turn's changed files: edits add up, a Write states the whole file, failed edits do not count", () => {
  seq = 0;
  const c = fold([
    ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "go" }),
    ev(1, { t: "tool_call", call_id: "e1", title: "Edit /w/a.py", kind: "edit", status: "completed", content: [{ type: "diff", path: "/w/a.py", old: "a\nb", new: "a\nB\nC" }] }),
    ev(1, { t: "tool_call", call_id: "e2", title: "Edit /w/a.py", kind: "edit", status: "completed", content: [{ type: "diff", path: "/w/a.py", old: "x", new: "y" }] }),
    ev(1, { t: "tool_call", call_id: "w1", title: "Write /w/new/a.py", kind: "edit", status: "completed", content: [{ type: "diff", path: "/w/new/a.py", old: null, new: "1\n2" }] }),
    ev(1, { t: "tool_call", call_id: "e3", title: "Edit /w/c.py", kind: "edit", status: "failed", content: [{ type: "diff", path: "/w/c.py", old: "x", new: "y" }] }),
    ev(1, { t: "turn_end", status: "completed", duration_ms: 1 }),
  ]);
  const files = cl.turnChanges(c.turns[0]);
  assert.deepEqual(plain(files), [{ path: "/w/a.py", add: 3, del: 2, created: false }, { path: "/w/new/a.py", add: 2, del: 0, created: true }]);
  assert.equal(cl.showChanges(c.turns[0], cl.claudeRows(c.turns[0])), true);
  assert.deepEqual(plain([...cl.folderHints(files.map((f) => f.path))]), [["/w/a.py", "w"], ["/w/new/a.py", "new"]]);
  // One edit on its own: its row already shows it.
  seq = 0;
  const one = fold([ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "go" }),
    ev(1, { t: "tool_call", call_id: "e1", title: "Edit /w/a.py", kind: "edit", status: "completed", content: [{ type: "diff", path: "/w/a.py", old: "a", new: "b" }] }),
    ev(1, { t: "turn_end", status: "completed", duration_ms: 1 })]);
  assert.equal(cl.showChanges(one.turns[0], cl.claudeRows(one.turns[0])), false);
});

test("the task list: a window of twelve from the step before the one being worked on", () => {
  const steps = Array.from({ length: 20 }, (_, i) => ({ content: `s${i}`, status: i < 9 ? "completed" : i === 9 ? "in_progress" : "pending" }));
  assert.deepEqual(plain(cl.todoWindow(steps)), { start: 8, end: 20, before: 8, openAfter: 0, focus: 9 });
  const early = steps.map((s, i) => ({ ...s, status: i === 1 ? "in_progress" : i < 1 ? "completed" : "pending" }));
  assert.deepEqual(plain(cl.todoWindow(early)), { start: 0, end: 12, before: 0, openAfter: 8, focus: 1 });
  const done = steps.map((s) => ({ ...s, status: "completed" }));
  assert.equal(cl.todoWindow(done).start, 8, "all done: the window ends at the last step");
});

test("the task list shows while a turn runs, and after one only when it was stopped with steps left", () => {
  seq = 0;
  const plan = (statuses) => ev(1, { t: "plan", entries: statuses.map((s, i) => ({ content: `s${i}`, status: s })) });
  let c = fold([ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "go" }), plan(["completed", "in_progress"])]);
  assert.equal(cl.planShown(c), true);
  c = chat.reduceChat(c, ev(1, { t: "turn_end", status: "completed", duration_ms: 1 }));
  assert.equal(cl.planShown(c), false);
  seq = 0;
  c = fold([ev(0, { t: "user", input_id: "u", by: { device: "d" }, text: "go" }), plan(["completed", "pending"]), ev(1, { t: "turn_end", status: "interrupted", duration_ms: 1 })]);
  assert.equal(cl.planShown(c), true, "stopped part way: the steps left stay in view");
  assert.equal(cl.latestPlan(c).entries.length, 2);
});

test("modes, the queue's preview and scroll keys", () => {
  assert.deepEqual(plain(cl.MODES), ["default", "acceptEdits", "plan", "bypassPermissions"]);
  assert.equal(cl.modeChange("plan", true), "ok");
  assert.equal(cl.modeChange("bypassPermissions", false), "confirm");
  assert.equal(cl.modeChange("bypassPermissions", true), "busy");
  assert.equal(cl.queuePreview("  two\n\nlines  "), "two lines");
  assert.equal(cl.queuePreview(" \n "), "…");
  assert.equal(cl.scrollKey("PageUp", false), "up");
  assert.equal(cl.scrollKey(" ", true), "up");
  assert.equal(cl.scrollKey(" ", false), "down");
  assert.equal(cl.scrollKey("a", false), null);
});
