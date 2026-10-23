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

// Exercise the real component's event handlers and rendered props without a browser. Effects are
// deliberately idle: these checks concern draft state and submission, not layout or native focus.
function viewHarness(file, deps) {
  let slot = 0;
  const slots = [];
  const hooks = {
    useState(initial) {
      const i = slot++;
      if (!(i in slots)) slots[i] = typeof initial === "function" ? initial() : initial;
      return [slots[i], (value) => { slots[i] = typeof value === "function" ? value(slots[i]) : value; }];
    },
    useRef(initial) {
      const i = slot++;
      return slots[i] ??= { current: initial };
    },
    useMemo: (fn) => fn(), useCallback: (fn) => fn, useEffect() {}, useLayoutEffect() {},
  };
  const jsx = (type, props) => ({ type, props });
  const source = readFileSync(new URL(`../apps/desktop/src/ui/${file}`, import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX,
  } }).outputText;
  const exports = {};
  const require = (name) => {
    if (name === "react") return hooks;
    if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
    if (name === "lucide-react") return {};
    if (name === "../util" || name === "./util") return { cx: (...parts) => parts.filter(Boolean).join(" ") };
    if (name === "../../lib/i18n" || name === "../lib/i18n") return { t: (key) => key };
    if (name in deps) return deps[name];
    throw new Error(`unexpected view dependency ${name}`);
  };
  vm.runInNewContext(compiled, { exports, require, Date, Math, Set, Map, Promise, setTimeout, clearTimeout,
    document: { documentElement: { dataset: { os: "macos" } } }, navigator: { platform: "MacIntel" },
  }, { filename: file });
  return { exports, render(fn, props) { slot = 0; return fn(props); } };
}

function findView(tree, match) {
  if (!tree || typeof tree !== "object") return null;
  if (Array.isArray(tree)) return tree.map((child) => findView(child, match)).find(Boolean) ?? null;
  return match(tree) ? tree : findView(tree.props?.children, match);
}

test("before the first connection, the composer keeps a typed draft and blocks both Enter and Send", () => {
  const h = viewHarness("chat/Composer.tsx", { "../../lib/chat": chat, "../../lib/chat-input": input });
  const sent = [];
  const props = { agent: "Claude", host: "test", running: false, canSteer: true, disabled: null,
    disabledSend: true, context: null, queued: [], history: [], variant: "claude",
    onSend: (...args) => sent.push(args), onStop() {}, onDequeue() {}, onEditQueued() {}, onSteerQueued() {},
  };
  const render = () => h.render(h.exports.Composer, props);
  const area = (tree) => findView(tree, (el) => el.type === "textarea").props;
  const send = (tree) => findView(tree, (el) => el.type === "button" && el.props.className === "round send").props;
  area(render()).onChange({ target: { value: "keep this draft", selectionStart: 15 } });
  let tree = render();
  assert.equal(area(tree).disabled, false, "typing stays available while connecting");
  assert.equal(send(tree).disabled, true);
  area(tree).onKeyDown({ key: "Enter", nativeEvent: {}, currentTarget: {}, preventDefault() {} });
  send(tree).onClick({ stopPropagation() {} });
  assert.equal(sent.length, 0, "both submission paths enforce the gate");
  assert.equal(area(render()).value, "keep this draft", "a blocked submit cannot clear the draft");
  props.disabledSend = false;
  tree = render();
  assert.equal(send(tree).disabled, false);
  area(tree).onKeyDown({ key: "Enter", nativeEvent: {}, currentTarget: {}, preventDefault() {} });
  assert.equal(sent.length, 1);
  assert.equal(sent[0][0], "keep this draft");
  assert.equal(area(render()).value, "");
});

test("Claude gates only the first hello; a later disconnection keeps the undelivered-message path", () => {
  const Composer = () => null;
  const conv = { chat: chat.emptyChat(), hello: null, connected: false, error: null, outgoing: [] };
  const h = viewHarness("claude/ClaudeConversation.tsx", {
    "../../lib/chat": chat, "../../lib/chat-input": input, "../../lib/codex": { isCodexRuntime: () => false },
    "../../lib/claude": { STRAY_ENTER_MS: 250 },
    "../AgentChat": { useConversation: () => conv, sentBySession: new Map(), useNowTick: () => 0 },
    "../chat/Composer": { Composer }, "../chat/FolderPicker": {}, "../chat/Preview": {}, "../Icon": {},
    "./Cards": {}, "./ClaudeTurn": { openStore: () => ({}) }, "./ClaudeTranscript": {}, "./Dock": {},
  });
  const props = { host: "test", target: { session: "one" }, onLeave() {} };
  const composerProps = () => {
    const view = h.render(h.exports.ClaudeConversation, props);
    return findView(h.render(view.type, view.props), (el) => el.type === Composer).props;
  };
  assert.equal(composerProps().disabledSend, true);
  assert.equal(composerProps().disabled, null);
  conv.hello = { role: "rw", runtime: { id: "claude", name: "Claude" }, caps: {} };
  conv.connected = true;
  assert.equal(composerProps().disabledSend, false);
  conv.connected = false;
  assert.equal(composerProps().disabledSend, false, "reconnecting after hello may still create an undelivered message");
});

test("the shared conversation panel also gates the first hello, including its Codex handoff", () => {
  const Composer = () => null;
  const conv = { chat: chat.emptyChat(), hello: null, connected: false, error: null, outgoing: [] };
  const h = viewHarness("AgentChat.tsx", {
    "../lib/api": {}, "../lib/chat": chat, "../lib/chat-input": input,
    "../lib/codex": { isCodexRuntime: (runtime) => runtime?.id === "codex", pendingRequest: () => null },
    "./chat/Composer": { Composer }, "./chat/FolderPicker": {}, "./chat/Rows": {}, "./chat/Preview": {},
    "./chat/Transcript": {}, "./codex/CodexLiveStrip": { livePlan: () => null },
    "./codex/CodexRequestCard": {}, "./Icon": {}, "./claude/ClaudeConversation": {},
  });
  const props = { host: "test", target: { session: "one" }, onLeave() {}, conv };
  const composerProps = () => findView(h.render(h.exports.ConversationPanel, props), (el) => el.type === Composer).props;
  assert.equal(composerProps().disabledSend, true);
  conv.hello = { role: "rw", runtime: { id: "codex", name: "Codex" }, caps: {} };
  conv.connected = true;
  assert.equal(composerProps().variant, "codex");
  assert.equal(composerProps().disabledSend, false);
  conv.connected = false;
  assert.equal(composerProps().disabledSend, false);
});

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
  // The list sent right after the reply (unnumbered) fills in only what no event has said.
  const opened = chat.reduceChat(chat.emptyChat(), { seq: -1, ts: 0, turn: 0, t: "commands", commands: [{ name: "a", kind: "command", insert: "/a" }] });
  assert.equal(opened.commands[0].name, "a");
  assert.equal(chat.reduceChat(c, { seq: -1, ts: 0, turn: 0, t: "commands", commands: [] }).commands[0].insert, "/review");
  assert.equal(chat.chatLineKind({ t: "commands", commands: [] }), "commands");
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

// ---------------------------------------------------------------------------------------------
// Find, quote, usage

test("find matches case-insensitively and steps around", () => {
  assert.deepEqual(plain(input.findMatches("Export export EXPORT", "export")), [[0, 6], [7, 13], [14, 20]]);
  assert.deepEqual(plain(input.findMatches("aaa", "aa")), [[0, 2]]);
  assert.deepEqual(plain(input.findMatches("abc", "")), []);
  assert.equal(input.stepMatch(-1, 3, 1), 0);
  assert.equal(input.stepMatch(-1, 3, -1), 2);
  assert.equal(input.stepMatch(2, 3, 1), 0);
  assert.equal(input.stepMatch(0, 0, 1), -1);
});

test("a selection becomes a quote below what is typed", () => {
  assert.equal(input.quoteText("line one\n\nline two\n"), "> line one\n>\n> line two\n\n");
  assert.equal(input.withQuote("", "> a\n\n"), "> a\n\n");
  assert.equal(input.withQuote("my reply  ", "> a\n\n"), "my reply\n\n> a\n\n");
});

test("the usage breakdown lists only what the agent reported", () => {
  assert.deepEqual(plain(input.usageRows(null, { used: 42000, window: 200000 })), [{ key: "context", value: 42000 }, { key: "free", value: 158000 }]);
  const rows = plain(input.usageRows({ input: 41200, cacheRead: 40000, cacheWrite: 0, output: 600, cost: 0.0123 }, null));
  assert.deepEqual(rows.map((r) => r.key), ["input", "cache_read", "output", "cost"]);
  assert.equal(rows[1].sub, true);
  seq = 0;
  const c = fold([ev(1, { t: "turn_start" }), ev(1, { t: "usage", input_tokens: 10, output_tokens: 2, cache_read_tokens: 8, reasoning_tokens: 1 }),
    ev(2, { t: "turn_start" }), ev(2, { t: "usage", cost_usd: 0.1 })]);
  // A later report without token counts keeps the last breakdown.
  assert.deepEqual(plain(c.usage), { input: 10, output: 2, cacheRead: 8, reasoning: 1 });
});

// ---------------------------------------------------------------------------------------------
// Sub-agents

test("a sub-agent's own tool calls fold into its row instead of the turn's runs", () => {
  seq = 0;
  const c = fold([
    ev(0, { t: "user", input_id: "u1", by: { device: "d" }, text: "go" }),
    ev(1, { t: "turn_start" }),
    ev(1, { t: "tool_call", call_id: "r1", title: "Read a.py", kind: "read", status: "completed" }),
    ev(1, { t: "tool_call", call_id: "a1", title: "Agent Find the date code", kind: "think", status: "in_progress" }),
    ev(1, { t: "tool_call", call_id: "g1", parent: "a1", title: "Grep monthrange", kind: "search", status: "in_progress" }),
    ev(1, { t: "tool_call", call_id: "b1", parent: "a1", title: "Bash ls", kind: "execute", status: "in_progress" }),
    ev(1, { t: "tool_update", call_id: "g1", status: "completed" }),
    ev(1, { t: "tool_call", call_id: "x9", parent: "nobody", title: "Bash pwd", kind: "execute", status: "completed" }),
  ]);
  const rows = plain(chat.turnRows(c.turns[0]));
  assert.deepEqual(rows.map((r) => r.kind), ["user", "run", "agent", "run"]);
  const agent = rows[2];
  assert.deepEqual(agent.children.map((x) => [x.callId, x.status]), [["g1", "completed"], ["b1", "in_progress"]]);
  // A call naming a parent that is not a sub-agent of this turn stays in the turn.
  assert.deepEqual(rows[3].tools.map((x) => x.callId), ["x9"]);
});
