// How a Claude (or ACP agent) conversation is laid out in the Claude view (ui/claude): a turn's rows
// (the user's message, prose, tool work and reasoning grouped and summed up in one sentence, sub-agents,
// answered questions), how a tool call reads ("Reading export.py", "Ran python export.py…"), what an
// approval card asks and which keys answer it, a question's pages and answers, the status line under
// the transcript, how a streamed reply is paced, the turn's changed files, and the task list window.
// The behaviour follows the clean-room notes of Claude's desktop app (`~/ar-refs/spec/claude-{TR,TC,CP}.md`,
// rule ids in the comments); the words are this project's own. Pure, tested in tools/claude-view.test.mjs.

import {
  baseName,
  diffRows,
  diffStat,
  isSubagent,
  type Chat,
  type DiffHunk,
  type PlanEntry,
  type QuestionBlock,
  type QuestionItem,
  type Request,
  type ThoughtBlock,
  type ToolContent,
  type ToolItem,
  type Turn,
} from "./chat";
import type { Attachment } from "./chat-input";

const str = (v: unknown): string => (typeof v === "string" ? v : "");
const running = (x: ToolItem) => x.status === "in_progress" || x.status === "pending";

// ---------------------------------------------------------------------------------------------
// What a tool call is

/** Claude Code's own tools, as the host names them at the start of a title. */
const CLAUDE_TOOLS = new Set([
  "Bash", "PowerShell", "BashOutput", "KillShell", "KillBash", "Read", "NotebookRead", "Write", "Edit", "MultiEdit", "NotebookEdit",
  "Grep", "Glob", "LS", "WebFetch", "WebSearch", "Task", "Agent", "TodoWrite", "ExitPlanMode", "EnterPlanMode", "AskUserQuestion",
  "Skill", "SlashCommand", "ListMcpResourcesTool", "ReadMcpResourceTool",
]);

/** The tool by the name Claude gave it: the first word of its title ("Bash", "Read",
 *  "mcp__github__get_issue"; another tool's title is its name alone), or "" for an agent whose titles
 *  are free text ("Reading file a.txt"). */
export function toolName(x: ToolItem): string {
  const t = x.title.trim();
  const w = t.split(/\s+/)[0] ?? "";
  if (CLAUDE_TOOLS.has(w) || /^mcp__\w/.test(w)) return w;
  return x.tool === "other" && /^[A-Z][A-Za-z0-9_]*$/.test(t) ? t : "";
}

/** The title after the tool's name ("python export.py" for "Bash python export.py"). */
export function toolArg(x: ToolItem): string {
  const name = toolName(x);
  const t = x.title.trim();
  return name ? t.slice(name.length).trim() : t;
}

/** A call's input as an object (the host may leave it out, or send a preview). */
export function inputOf(x: { input?: unknown }): Record<string, unknown> {
  return x.input && typeof x.input === "object" && !Array.isArray(x.input) ? (x.input as Record<string, unknown>) : {};
}

/** The file a call acted on: where the host says it was, its diff's path, or the title's argument. */
export function toolPath(x: ToolItem): string {
  const diff = x.content.find((c) => c.type === "diff") as Extract<ToolContent, { type: "diff" }> | undefined;
  return x.locations[0] || diff?.path || str(inputOf(x).file_path) || toolArg(x);
}

/** What a group's sentence counts a call as (TR-29); file kinds count distinct files. */
export type Cat = "read" | "create" | "edit" | "delete" | "command" | "search" | "fetch" | "websearch" | "agent" | "todo" | "plan" | "question" | "skill" | "tool";
const FILE_CATS = new Set<Cat>(["read", "create", "edit", "delete"]);

/** A Write over a file that was there counts as an edit (its patch has old lines). */
function overwrote(x: ToolItem): boolean {
  return x.content.some((c) => c.type === "diff" && ((c.old !== null && c.old !== "") || !!c.hunks?.some((h) => h.lines.some((l) => l[0] === "-" || l[0] === " "))));
}

export function toolCat(x: ToolItem): Cat {
  switch (toolName(x)) {
    case "Read":
    case "NotebookRead":
      return "read";
    case "Write":
      return overwrote(x) ? "edit" : "create";
    case "Edit":
    case "MultiEdit":
    case "NotebookEdit":
      return "edit";
    case "Bash":
    case "PowerShell":
    case "BashOutput":
      return "command";
    case "Grep":
    case "Glob":
    case "LS":
      return "search";
    case "WebFetch":
      return "fetch";
    case "WebSearch":
      return "websearch";
    case "Task":
    case "Agent":
      return "agent";
    case "TodoWrite":
      return "todo";
    case "ExitPlanMode":
    case "EnterPlanMode":
      return "plan";
    case "AskUserQuestion":
      return "question";
    case "Skill":
      return "skill";
  }
  if (toolName(x)) return "tool";
  // An agent without Claude's tool names (ACP): by the kind the host gave.
  switch (x.tool) {
    case "execute":
      return "command";
    case "read":
      return "read";
    case "edit":
    case "move":
      return x.content.some((c) => c.type === "diff" && (c.old === null || c.old === "")) ? "create" : "edit";
    case "delete":
      return "delete";
    case "search":
      return "search";
    case "fetch":
      return "fetch";
    case "think":
      return "agent";
    default:
      return "tool";
  }
}

// ---------------------------------------------------------------------------------------------
// The state a call shows (TC-11), and the permission it waits on (TC-44)

export type ToolState = "busy" | "done" | "failed" | "denied" | "stopped" | "blocked";

/** The fixed words Claude Code's tool result has when a person said no, or the turn was stopped. */
const DENIED_TEXT = /doesn't want to proceed with this tool use|tool use was rejected|permission to use \S+ has been denied/i;
const INTERRUPTED_TEXT = /\[Request interrupted by user/i;

/**
 * Which call each open or settled permission of a turn is about: by the call id the host names, or
 * (an older host) the newest call of that tool in the turn with the same input, or the newest running
 * one of that tool.
 */
export function requestsByCall(turn: Turn, requests: Request[]): Map<string, Request> {
  const out = new Map<string, Request>();
  const tools = turn.blocks.filter((b): b is ToolItem => b.kind === "tool");
  for (const r of requests) {
    if (r.kind !== "permission" || r.turn !== turn.n) continue;
    if (r.callId) {
      out.set(r.callId, r);
      continue;
    }
    const same = (x: ToolItem) => toolName(x) === r.tool && !out.has(x.callId);
    const wanted = r.input !== undefined ? JSON.stringify(r.input) : null;
    const hit = [...tools].reverse().find((x) => same(x) && wanted !== null && JSON.stringify(x.input) === wanted)
      ?? [...tools].reverse().find((x) => same(x) && running(x));
    if (hit) out.set(hit.callId, r);
  }
  return out;
}

export function toolState(x: ToolItem, req?: Request): ToolState {
  if (req && !req.resolved) return "blocked";
  if (req?.resolved?.outcome === "denied") return "denied";
  if (running(x)) return "busy";
  if (x.stopped) return "stopped";
  if (x.status === "failed") {
    const out = x.content.map((c) => (c.type === "text" ? c.text : "")).join("\n");
    if (DENIED_TEXT.test(out)) return "denied";
    if (INTERRUPTED_TEXT.test(out)) return "stopped";
    return "failed";
  }
  return "done";
}

// ---------------------------------------------------------------------------------------------
// A turn's rows (TR-08, TR-28): prose breaks the work into groups; reasoning and tool calls in a row
// form one group; a sub-agent is its own row with its own calls inside.

export type ClRow =
  | { kind: "user"; key: string; text: string; by: string; ts: number; steer: boolean; inputId?: string; attachments?: Attachment[] }
  | { kind: "text"; key: string; id: string; text: string; streaming: boolean }
  | { kind: "group"; key: string; members: Array<ToolItem | ThoughtBlock>; tools: ToolItem[]; live: boolean }
  | { kind: "agent"; key: string; tool: ToolItem; children: ToolItem[] }
  | { kind: "question"; key: string; block: QuestionBlock }
  | { kind: "notice"; key: string; level: string; code: string; text: string };

export function claudeRows(turn: Turn): ClRow[] {
  const rows: ClRow[] = [];
  if (turn.user) {
    rows.push({ kind: "user", key: `u${turn.n}`, text: turn.user.text, by: turn.user.by, ts: turn.user.ts, steer: false,
      inputId: turn.user.inputId, attachments: turn.user.attachments });
  }
  const b = turn.blocks;
  const agents = new Set(b.filter((x): x is ToolItem => x.kind === "tool" && isSubagent(x)).map((x) => x.callId));
  const children = new Map<string, ToolItem[]>();
  for (const x of b) if (x.kind === "tool" && x.parent && agents.has(x.parent)) children.set(x.parent, [...(children.get(x.parent) ?? []), x]);
  let group: Extract<ClRow, { kind: "group" }> | null = null;
  b.forEach((x, i) => {
    if (x.kind === "tool" && x.parent && agents.has(x.parent)) return;
    if ((x.kind === "tool" && !isSubagent(x)) || x.kind === "thought") {
      if (x.kind === "thought" && !x.text.trim()) return;
      if (!group) {
        group = { kind: "group", key: `g${turn.n}:${x.kind === "tool" ? x.callId : x.id}`, members: [], tools: [], live: false };
        rows.push(group);
      }
      group.members.push(x);
      if (x.kind === "tool") group.tools.push(x);
      return;
    }
    // The to-do list is shown above the composer; it does not break the work it is part of.
    if (x.kind === "plan") return;
    if (x.kind === "text" && !x.text.trim()) return;
    group = null;
    switch (x.kind) {
      case "text":
        rows.push({ kind: "text", key: `m${turn.n}:${x.id}`, id: x.id, text: x.text, streaming: false });
        break;
      case "tool":
        rows.push({ kind: "agent", key: `a${turn.n}:${x.callId}`, tool: x, children: children.get(x.callId) ?? [] });
        break;
      case "question":
        rows.push({ kind: "question", key: `q${turn.n}:${x.qid}`, block: x });
        break;
      case "notice":
        rows.push({ kind: "notice", key: `n${turn.n}:${i}`, level: x.level, code: x.code, text: x.text });
        break;
      case "user":
        rows.push({ kind: "user", key: `s${turn.n}:${i}`, text: x.text, by: x.by, ts: x.ts, steer: true, inputId: x.inputId, attachments: x.attachments });
        break;
    }
  });
  if (turn.status === "running") {
    const last = rows[rows.length - 1];
    if (last?.kind === "text") last.streaming = true;
    if (last?.kind === "group") last.live = true;
    for (const r of rows) if (r.kind === "group" && r.tools.some(running)) r.live = true;
  }
  return rows;
}

/** A finished group of one call and nothing else is shown as that call's own row (TR-28). */
export function singleCall(row: Extract<ClRow, { kind: "group" }>): ToolItem | null {
  return !row.live && row.members.length === 1 && row.tools.length === 1 ? row.tools[0] : null;
}

// ---------------------------------------------------------------------------------------------
// A group's sentence (TR-29, TR-30, TC-17): "Read 2 files, ran 3 commands, edited 1 file (1 failed)"

export interface Phrase {
  cat: Cat;
  n: number;
  failed: number;
  blocked: number;
  stopped: number;
  denied: number;
  /** Every call of the kind failed: the phrase says so instead. */
  error: boolean;
}

export interface Sentence {
  phrases: Phrase[];
  /** The kinds past the first three when there are five or more, summed up. */
  more: { n: number; failed: number; blocked: number } | null;
  /** All the work was on one file: "read and edited export.py" (the verbs in the order they happened). */
  file: { path: string; cats: Cat[]; preferDiff: boolean } | null;
}

/** Phrases shown in full up to this many; from one more, the first `LEAD` and "N more". */
const MAX_PHRASES = 4;
const LEAD = 3;

export function groupSentence(tools: ToolItem[], states: Map<string, ToolState> = new Map()): Sentence {
  const by = new Map<Cat, { phrase: Phrase; seen: Set<string> }>();
  const order: Cat[] = [];
  for (const x of tools) {
    const cat = toolCat(x);
    let e = by.get(cat);
    if (!e) {
      e = { phrase: { cat, n: 0, failed: 0, blocked: 0, stopped: 0, denied: 0, error: false }, seen: new Set() };
      by.set(cat, e);
      order.push(cat);
    }
    const key = FILE_CATS.has(cat) ? toolPath(x) || x.callId : x.callId;
    if (!e.seen.has(key)) {
      e.seen.add(key);
      e.phrase.n++;
    }
    const st = states.get(x.callId) ?? toolState(x);
    if (st === "failed") e.phrase.failed++;
    else if (st === "blocked") e.phrase.blocked++;
    else if (st === "stopped") e.phrase.stopped++;
    else if (st === "denied") e.phrase.denied++;
  }
  // Calls the sentence has no word for come last ("used 2 tools").
  const cats = [...order.filter((c) => c !== "tool"), ...order.filter((c) => c === "tool")];
  const phrases = cats.map((c) => {
    const p = by.get(c)!.phrase;
    const calls = tools.filter((x) => toolCat(x) === c).length;
    return { ...p, error: p.failed > 0 && p.failed === calls };
  });
  let shown = phrases;
  let more: Sentence["more"] = null;
  if (phrases.length > MAX_PHRASES) {
    shown = phrases.slice(0, LEAD);
    const rest = phrases.slice(LEAD);
    more = { n: rest.reduce((a, p) => a + p.n, 0), failed: rest.reduce((a, p) => a + p.failed + p.denied, 0), blocked: rest.reduce((a, p) => a + p.blocked, 0) };
  }
  let file: Sentence["file"] = null;
  const paths = new Set(tools.map((x) => toolPath(x)));
  if (tools.length && tools.every((x) => FILE_CATS.has(toolCat(x)) && toolPath(x)) && paths.size === 1 && !phrases.some((p) => p.error)) {
    file = { path: [...paths][0], cats: order, preferDiff: tools.some((x) => (toolCat(x) === "edit" || toolCat(x) === "create") && (states.get(x.callId) ?? toolState(x)) === "done") };
  }
  return { phrases: shown, more, file };
}

/** Lines added and removed by a group's edits (null when it changed nothing). */
export function groupStat(tools: ToolItem[]): { add: number; del: number } | null {
  let add = 0;
  let del = 0;
  let any = false;
  for (const x of tools) {
    for (const c of x.content) {
      if (c.type !== "diff") continue;
      const st = contentStat(c);
      any = true;
      add += st.add;
      del += st.del;
    }
  }
  return any ? { add, del } : null;
}

// ---------------------------------------------------------------------------------------------
// One call's words (TC-01 … TC-10): a verb in three tenses and what it acted on.

export type Tense = "busy" | "done" | "failed";
/** The verb families the strings have (`cl_t_<verb>_<tense>`). */
export type Verb = "read" | "create" | "write" | "edit" | "delete" | "command" | "search" | "list" | "fetch" | "websearch" | "agent" | "todo" | "todo_clear"
  | "plan_exit" | "plan_enter" | "question" | "questions" | "skill" | "use";

export interface Words {
  /** The verb, or null when `text` is the whole phrase (a command's own description). */
  verb: Verb | null;
  /** The phrase itself when the agent wrote one ("Running the test suite"). */
  text?: string;
  /** For a failed call with its own description: the phrase after "Failed to" ("run the test suite"). */
  failedText?: string;
  obj: string;
  /** How the object shows: monospace (`code`), a file name (`file`), or as part of the sentence. */
  objKind: "code" | "file" | "plain" | null;
  /** The full path of a file object (its tooltip). */
  path?: string;
  /** For a question: how many it asked. */
  n?: number;
}

export function tenseOf(st: ToolState): Tense {
  return st === "busy" || st === "blocked" ? "busy" : st === "failed" ? "failed" : "done";
}

/** "mcp__github__get_issue" as "github: get issue". */
export function mcpLabel(name: string): string {
  const m = /^mcp__(.+?)__(.+)$/.exec(name);
  if (!m) return name.split("__").pop() ?? name;
  return `${m[1].replace(/_/g, " ")}: ${m[2].replace(/_/g, " ")}`;
}

export function toolWords(x: ToolItem, tense: Tense): Words {
  const name = toolName(x);
  const input = inputOf(x);
  const path = toolPath(x);
  const file = (verb: Verb): Words => ({ verb, obj: baseName(path), objKind: "file", path });
  switch (name) {
    case "Bash":
    case "PowerShell": {
      const desc = str(input.description).trim();
      if (desc) {
        const d = describe(desc, tense);
        if (d.known) return tense === "failed" ? { verb: null, failedText: d.text, obj: "", objKind: null } : { verb: null, text: d.text, obj: "", objKind: null };
      }
      const cmd = str(input.command) || toolArg(x);
      const sum = commandSummary(cmd);
      // A description in no form the verbs know is shown as written.
      if (desc && !sum) return { verb: null, text: desc, obj: "", objKind: null };
      return { verb: "command", obj: sum ?? "", objKind: sum ? "code" : null };
    }
    case "Read":
    case "NotebookRead":
      return file("read");
    case "Write":
      return file(overwrote(x) ? "write" : "create");
    case "Edit":
    case "MultiEdit":
    case "NotebookEdit":
      return file("edit");
    case "Grep":
    case "Glob":
      return { verb: "search", obj: str(input.pattern) || toolArg(x), objKind: "plain" };
    case "LS":
      return { verb: "list", obj: str(input.path) || toolArg(x), objKind: "plain" };
    case "WebFetch":
      return { verb: "fetch", obj: str(input.url) || toolArg(x), objKind: "plain" };
    case "WebSearch":
      return { verb: "websearch", obj: str(input.query) || toolArg(x), objKind: "plain" };
    case "Task":
    case "Agent":
      return { verb: "agent", obj: str(input.description) || toolArg(x), objKind: "plain" };
    case "TodoWrite":
      return { verb: Array.isArray(input.todos) && input.todos.length ? "todo" : "todo_clear", obj: "", objKind: null };
    case "AskUserQuestion": {
      const qs = Array.isArray(input.questions) ? (input.questions as Array<{ header?: unknown }>) : [];
      const header = str(qs[0]?.header);
      return header || qs.length < 2 ? { verb: "question", obj: header, objKind: "plain" } : { verb: "questions", obj: "", objKind: null, n: qs.length };
    }
    case "ExitPlanMode":
      return { verb: "plan_exit", obj: "", objKind: null };
    case "EnterPlanMode":
      return { verb: "plan_enter", obj: "", objKind: null };
    case "Skill": {
      const skill = str(input.skill) || str(input.command) || toolArg(x);
      return { verb: "skill", obj: skill ? `/${skill.replace(/^\//, "")}` : "", objKind: "code" };
    }
  }
  if (name) return { verb: "use", obj: name.startsWith("mcp__") ? mcpLabel(name) : name, objKind: "plain" };
  // An agent without Claude's tool names: by kind, with its title as the object.
  const arg = x.title.trim();
  switch (x.tool) {
    case "execute": {
      const cmd = str(input.command) || arg;
      return { verb: "command", obj: commandSummary(cmd) ?? "", objKind: "code" };
    }
    case "read":
      return { verb: "read", obj: arg, objKind: "plain" };
    case "edit":
    case "move":
      return { verb: "edit", obj: arg, objKind: "plain" };
    case "delete":
      return { verb: "delete", obj: arg, objKind: "plain" };
    case "search":
      return { verb: "search", obj: arg, objKind: "plain" };
    case "fetch":
      return { verb: "fetch", obj: arg, objKind: "plain" };
    default:
      return { verb: "use", obj: arg, objKind: "plain" };
  }
}

// ---- A command's own description in the right tense (TC-02) ----------------------------------

/** Verbs that start command descriptions: base → [-ing, past]. */
const VERBS: Record<string, [string, string]> = {
  add: ["adding", "added"], analyze: ["analyzing", "analyzed"], apply: ["applying", "applied"], build: ["building", "built"],
  bump: ["bumping", "bumped"], calculate: ["calculating", "calculated"], change: ["changing", "changed"], check: ["checking", "checked"],
  checkout: ["checking out", "checked out"], clean: ["cleaning", "cleaned"], clear: ["clearing", "cleared"], clone: ["cloning", "cloned"],
  commit: ["committing", "committed"], compare: ["comparing", "compared"], compile: ["compiling", "compiled"], compress: ["compressing", "compressed"],
  configure: ["configuring", "configured"], convert: ["converting", "converted"], copy: ["copying", "copied"], count: ["counting", "counted"],
  create: ["creating", "created"], debug: ["debugging", "debugged"], delete: ["deleting", "deleted"], deploy: ["deploying", "deployed"],
  diff: ["diffing", "diffed"], display: ["displaying", "displayed"], download: ["downloading", "downloaded"], dump: ["dumping", "dumped"],
  echo: ["echoing", "echoed"], edit: ["editing", "edited"], execute: ["executing", "executed"], explore: ["exploring", "explored"],
  export: ["exporting", "exported"], extract: ["extracting", "extracted"], fetch: ["fetching", "fetched"], filter: ["filtering", "filtered"],
  find: ["finding", "found"], fix: ["fixing", "fixed"], format: ["formatting", "formatted"], generate: ["generating", "generated"],
  get: ["getting", "got"], grep: ["grepping", "grepped"], import: ["importing", "imported"], initialize: ["initializing", "initialized"],
  inspect: ["inspecting", "inspected"], install: ["installing", "installed"], kill: ["killing", "killed"], launch: ["launching", "launched"],
  lint: ["linting", "linted"], list: ["listing", "listed"], load: ["loading", "loaded"], locate: ["locating", "located"],
  look: ["looking", "looked"], make: ["making", "made"], measure: ["measuring", "measured"], merge: ["merging", "merged"],
  move: ["moving", "moved"], open: ["opening", "opened"], parse: ["parsing", "parsed"], ping: ["pinging", "pinged"],
  print: ["printing", "printed"], pull: ["pulling", "pulled"], push: ["pushing", "pushed"], query: ["querying", "queried"],
  read: ["reading", "read"], rebase: ["rebasing", "rebased"], rebuild: ["rebuilding", "rebuilt"], remove: ["removing", "removed"],
  rename: ["renaming", "renamed"], render: ["rendering", "rendered"], reset: ["resetting", "reset"], restart: ["restarting", "restarted"],
  restore: ["restoring", "restored"], retry: ["retrying", "retried"], revert: ["reverting", "reverted"], review: ["reviewing", "reviewed"],
  run: ["running", "ran"], save: ["saving", "saved"], scan: ["scanning", "scanned"], search: ["searching", "searched"],
  set: ["setting", "set"], show: ["showing", "showed"], sort: ["sorting", "sorted"], start: ["starting", "started"],
  stash: ["stashing", "stashed"], stop: ["stopping", "stopped"], sync: ["syncing", "synced"], tail: ["tailing", "tailed"],
  test: ["testing", "tested"], touch: ["touching", "touched"], trace: ["tracing", "traced"], try: ["trying", "tried"],
  uninstall: ["uninstalling", "uninstalled"], unzip: ["unzipping", "unzipped"], update: ["updating", "updated"], upgrade: ["upgrading", "upgraded"],
  upload: ["uploading", "uploaded"], use: ["using", "used"], validate: ["validating", "validated"], verify: ["verifying", "verified"],
  view: ["viewing", "viewed"], wait: ["waiting", "waited"], watch: ["watching", "watched"], write: ["writing", "wrote"],
};
const PREFIXES = ["re-", "un-", "pre-", "de-", "co-"];

/** A verb (possibly `re-run`) in the tense; null when it is not one the table knows. */
function verbIn(word: string, tense: Tense): string | null {
  const lower = word.toLowerCase();
  const pre = PREFIXES.find((p) => lower.startsWith(p) && VERBS[lower.slice(p.length)]) ?? "";
  const forms = VERBS[lower.slice(pre.length)];
  if (!forms) return null;
  const out = pre + (tense === "busy" ? forms[0] : tense === "done" ? forms[1] : lower.slice(pre.length));
  return word[0] === word[0].toUpperCase() && word[0] !== word[0].toLowerCase() ? out[0].toUpperCase() + out.slice(1) : out;
}

/**
 * A command description ("Run the test suite") as the row says it: running ("Running the test
 * suite"), done ("Ran the test suite") or, failed, the words after "Failed to" ("run the test
 * suite"). Verbs after "and" / "then" change too, unless what follows is "is" / "are" / "was" / "were".
 * `known` is false when the first word is not a verb the table has: the text is then as written.
 */
export function describe(desc: string, tense: Tense): { text: string; known: boolean } {
  const m = /^([A-Za-z]+(?:-[A-Za-z]+)?)(.*)$/s.exec(desc.trim());
  const first = m && verbIn(m[1], tense);
  if (!m || !first) return { text: desc.trim(), known: false };
  const lead = tense === "failed" ? first.toLowerCase() : first;
  const rest = m[2].replace(/(?<![\w-])((?:[Aa]nd|[Tt]hen)\s+)([A-Za-z]+(?:-[A-Za-z]+)?)(?!\s+(?:is|are|was|were)\b)/g,
    (all, glue: string, word: string) => {
      const v = verbIn(word, tense);
      return v ? glue + v : all;
    });
  return { text: lead + rest, known: true };
}

// ---- A command as a few words (TC-02, TC-03) -------------------------------------------------

/** Programs whose first words can be shown; anything else is "a command". */
const SAFE_PROGRAMS = new Set([
  "bun", "bunx", "node", "npm", "npx", "pnpm", "yarn", "deno", "tsc", "vite", "jest", "vitest", "eslint", "prettier",
  "python", "python3", "pip", "pip3", "uv", "uvx", "poetry", "pytest", "ruff", "mypy", "black",
  "cargo", "rustc", "rustup", "go", "gofmt", "java", "javac", "mvn", "gradle", "dotnet", "swift", "xcodebuild", "make", "cmake", "ninja", "gcc", "clang",
  "git", "gh", "docker", "kubectl", "helm", "terraform",
  "ls", "dir", "cat", "head", "tail", "less", "wc", "grep", "rg", "find", "fd", "sed", "awk", "sort", "uniq", "cut", "tr", "diff", "jq",
  "echo", "printf", "pwd", "which", "where", "whoami", "date", "env", "tree", "stat", "file", "du", "df",
  "mkdir", "rmdir", "touch", "cp", "mv", "rm", "ln", "chmod", "tar", "zip", "unzip", "gzip",
  "ps", "kill", "pkill", "lsof", "top", "uname", "sleep", "true", "false", "test", "open", "code",
  "cd", "tee", "xargs", "basename", "dirname", "realpath", "column", "comm", "paste", "seq", "timeout", "nohup", "more",
  "sha256sum", "shasum", "md5sum", "xxd", "hexdump", "od", "strings", "otool", "nm", "objdump",
]);
/** Words that make a command not shown at all (credentials, tokens). */
const SENSITIVE = [
  /:\/\/[^\s/@]+@/,
  /(?:^|\s)(?:-u|--user|--proxy-user)\s+\S{1,256}:\S/,
  /\b(?:authorization|bearer|sshpass)\b/i,
  /\b(?:mysql|mariadb)\b[\s\S]{0,512}\s-p\S*/,
  /\b(?:npm_|hf_|sntry[su]_|glpat-|pypi-AgE)/,
  /(?:^|\s)(?:login|auth)(?:\s|$)/,
  /--?[\w-]*(?:password|passwd|token|secret|credential)[\w-]*/i,
  /\bsudo\s+(?:-\w*\s+)*-S\b/,
  /-pass(?:in|out)\b|\bpass:|\benv:/,
  /ConvertTo-SecureString|identified\s+by/i,
  /\b[\w-]*(?:token|secret|password|passphrase|pwd|apikey|api_key|auth|credential|cookie|session)[\w-]*\s*=/i,
  /\.claude[\\/]projects|agent-memory/,
];
const WORD_OK = /^[A-Za-z0-9._/~:+,@%-]+$/;

/** Whether a command must not be shown (a credential in it, or the agent's own memory). */
export function sensitiveCommand(cmd: string): boolean {
  return SENSITIVE.some((re) => re.test(cmd));
}

/**
 * The first words of a command for a row ("python export.py…"), or null when it should not be
 * shown: credentials, substitutions, a program not in the list, or nothing safe to show.
 */
export function commandSummary(cmd: string): string | null {
  const scan = cmd.slice(0, 8192);
  if (!scan.trim() || sensitiveCommand(scan) || /\$\(|`|<\(|>\(/.test(scan)) return null;
  // Every program of the pipeline must be one the list knows.
  for (const seg of scan.split(/\n|;|\|\||&&|\|/)) {
    const words = seg.trim().split(/\s+/).filter(Boolean);
    let k = 0;
    while (k < words.length && (/^[A-Za-z_]\w*=/.test(words[k]) || words[k] === "time" || words[k] === "nice")) k++;
    if (k < words.length && !SAFE_PROGRAMS.has(words[k].replace(/^.*[\\/]/, "").replace(/\.exe$/i, "").toLowerCase())) return null;
  }
  const lines = scan.split("\n");
  let line = lines[0].trim();
  for (let m = /^cd\s+\S+\s*(?:&&|;)\s*/.exec(line); m; m = /^cd\s+\S+\s*(?:&&|;)\s*/.exec(line)) line = line.slice(m[0].length);
  const words = line.split(/\s+/).filter(Boolean);
  const out: string[] = [];
  for (const w of words) {
    if (out.length === 4) break;
    const stop = !WORD_OK.test(w) || w.startsWith("-") || w.includes("://") || /^[\w.-]+:\d+/.test(w) || /^[\w-]+(?:\.[\w-]+)+\//.test(w)
      || /%[0-9A-Fa-f]{2}/.test(w) || /.@/.test(w) || /(?=[A-Za-z0-9]*\d)(?=[A-Za-z0-9]*[A-Za-z])[A-Za-z0-9]{20,}/.test(w)
      || /token|password|passwd|secret|credential|api[-_]?key/i.test(w);
    if (stop) break;
    out.push(w);
  }
  if (!out.length) return null;
  const cut = out.length < words.length || lines.slice(1).some((l) => l.trim());
  return out.join(" ") + (cut ? "…" : "");
}

// ---------------------------------------------------------------------------------------------
// What a call's body shows

export type BodyKind = "bash" | "read" | "diff" | "todo" | "plan" | "generic";

export function bodyKind(x: ToolItem): BodyKind {
  const name = toolName(x);
  if (x.content.some((c) => c.type === "diff")) return "diff";
  if (name === "Bash" || name === "PowerShell" || (!name && x.tool === "execute")) return "bash";
  if (name === "Read" || name === "NotebookRead") return "read";
  if (name === "TodoWrite") return "todo";
  if (name === "ExitPlanMode") return "plan";
  return "generic";
}

/** A Read's output as the file's text (TC-25): the reminder Claude appends is dropped, and the
 *  `cat -n` line numbers when every line has one; the first number is where the excerpt starts. */
export function readText(out: string): { text: string; first: number | null } {
  const cut = out.indexOf("\n<system-reminder>");
  const body = (cut >= 0 ? out.slice(0, cut) : out).replace(/\n+$/, "");
  const lines = body.split("\n");
  const re = /^ *(\d+)(?:[:|] ?|→|\t)/;
  const filled = lines.filter((l) => l.trim());
  if (!filled.length || !filled.every((l) => re.test(l))) return { text: body, first: null };
  const first = Number(re.exec(filled[0])![1]);
  return { text: lines.map((l) => l.replace(re, "")).join("\n"), first };
}

/** The key: value lines of a generic call's input (TC-31); objects as JSON. */
export function inputLines(input: unknown): Array<{ key: string; value: string; mono: boolean }> {
  const o = inputOf({ input });
  const MONO = new Set(["command", "cmd", "script", "shell", "code", "pattern", "regex", "glob"]);
  return Object.entries(o).map(([key, v]) => {
    let value: string;
    if (typeof v === "string") value = v;
    else {
      try {
        value = JSON.stringify(v);
      } catch {
        value = String(v);
      }
    }
    return { key, value, mono: MONO.has(key) };
  });
}

// ---- Diffs with real line numbers (TC-27, TC-28) ----

export type NumberedLine =
  | { kind: "context" | "add" | "del"; old: number | null; new: number | null; text: string }
  | { kind: "gap"; lines: number | null };

/** The lines of a patch's hunks, numbered; a gap between hunks says how many lines it skips. */
export function hunkLines(hunks: DiffHunk[]): NumberedLine[] {
  const out: NumberedLine[] = [];
  let prevOldEnd: number | null = null;
  for (const h of hunks) {
    if (prevOldEnd !== null) out.push({ kind: "gap", lines: Math.max(0, h.old_start - prevOldEnd) || null });
    let o = h.old_start;
    let n = h.new_start;
    for (const l of h.lines) {
      const tag = l[0];
      const text = l.slice(1);
      if (tag === "+") out.push({ kind: "add", old: null, new: n++, text });
      else if (tag === "-") out.push({ kind: "del", old: o++, new: null, text });
      else if (tag === " " || tag === undefined) out.push({ kind: "context", old: o++, new: n++, text });
      // "\ No newline at end of file" carries no line.
    }
    prevOldEnd = o;
  }
  return out;
}

/** Whether hunks are well formed (every line starts with " ", "+", "-" or "\"). */
export function hunksOk(hunks: DiffHunk[] | undefined): hunks is DiffHunk[] {
  return !!hunks?.length && hunks.every((h) => Number.isFinite(h.old_start) && Number.isFinite(h.new_start) && Array.isArray(h.lines)
    && h.lines.every((l) => typeof l === "string" && (l === "" || " +-\\".includes(l[0]))));
}

/** A diff as rows: numbered from its hunks when it has good ones, else computed from old and new. */
export function diffLines(c: Extract<ToolContent, { type: "diff" }>): { rows: NumberedLine[]; numbered: boolean } {
  if (hunksOk(c.hunks)) return { rows: hunkLines(c.hunks), numbered: true };
  return {
    rows: diffRows(c.old, c.new).map((r) => (r.kind === "gap" ? { kind: "gap", lines: r.lines } : { kind: r.kind, old: null, new: null, text: r.text })),
    numbered: false,
  };
}

export function contentStat(c: Extract<ToolContent, { type: "diff" }>): { add: number; del: number } {
  if (hunksOk(c.hunks)) {
    let add = 0;
    let del = 0;
    for (const h of c.hunks) {
      for (const l of h.lines) {
        if (l[0] === "+") add++;
        else if (l[0] === "-") del++;
      }
    }
    return { add, del };
  }
  return diffStat([c]) ?? { add: 0, del: 0 };
}

// ---------------------------------------------------------------------------------------------
// The turn's changed files (TR-40)

export interface FileChange {
  path: string;
  add: number;
  del: number;
  created: boolean;
}

const EDIT_TOOLS = new Set(["Write", "Edit", "MultiEdit", "NotebookEdit"]);
const isEditCall = (x: ToolItem) => EDIT_TOOLS.has(toolName(x)) || (!toolName(x) && x.content.some((c) => c.type === "diff"));

/** The files a turn changed, in the order it first touched them, with lines added and removed. */
export function turnChanges(turn: Turn): FileChange[] {
  const by = new Map<string, FileChange>();
  for (const b of turn.blocks) {
    if (b.kind !== "tool" || !isEditCall(b) || b.status === "failed") continue;
    for (const c of b.content) {
      if (c.type !== "diff") continue;
      const st = contentStat(c);
      const have = by.get(c.path);
      // A Write states the whole file: it replaces what was counted for that file before.
      if (have && toolName(b) !== "Write") {
        have.add += st.add;
        have.del += st.del;
      } else by.set(c.path, { path: c.path, add: st.add, del: st.del, created: !have && (c.old === null || c.old === "") && !overwrote(b) });
    }
  }
  return [...by.values()];
}

/** The card shows when the turn edited twice or more, or once inside a group of other work. */
export function showChanges(turn: Turn, rows: ClRow[]): boolean {
  if (turn.status === "running") return false;
  const edits = turn.blocks.filter((b): b is ToolItem => b.kind === "tool" && isEditCall(b) && b.status !== "failed");
  if (edits.length >= 2) return true;
  if (edits.length === 1) {
    const g = rows.find((r) => r.kind === "group" && r.tools.includes(edits[0]));
    return !!g && g.kind === "group" && !singleCall(g);
  }
  return false;
}

/** Files with the same name get the folder above them as a hint. */
export function folderHints(paths: string[]): Map<string, string> {
  const out = new Map<string, string>();
  const names = new Map<string, number>();
  for (const p of paths) names.set(baseName(p), (names.get(baseName(p)) ?? 0) + 1);
  for (const p of paths) {
    if ((names.get(baseName(p)) ?? 0) < 2) continue;
    const parts = p.split(/[\\/]/).filter(Boolean);
    out.set(p, parts.length > 1 ? parts[parts.length - 2] : "");
  }
  return out;
}

// ---------------------------------------------------------------------------------------------
// The task list above the composer (TR-42, TR-43)

/** The latest to-do list of the conversation, the turn it is from, and whether that turn changed it. */
export function latestPlan(chat: Chat): { entries: PlanEntry[]; turn: number; current: boolean } | null {
  for (let i = chat.turns.length - 1; i >= 0; i--) {
    const p = chat.turns[i].blocks.find((b) => b.kind === "plan");
    if (p && p.kind === "plan" && p.entries.length) return { entries: p.entries, turn: chat.turns[i].n, current: i === chat.turns.length - 1 };
  }
  return null;
}

/** Whether the list shows: while a turn runs unless all of it was done before; after, only when that
 *  turn left steps open (it was stopped part way). */
export function planShown(chat: Chat): boolean {
  const p = latestPlan(chat);
  if (!p) return false;
  const last = chat.turns[chat.turns.length - 1];
  const allDone = p.entries.every((e) => e.status === "completed");
  if (last?.status === "running") return !(allDone && !p.current);
  return p.current && last?.status === "interrupted" && !allDone;
}

/** The window of `size` steps around the step being worked on: from the one before it. */
export function todoWindow(entries: PlanEntry[], size = 12): { start: number; end: number; before: number; openAfter: number; focus: number } {
  let focus = entries.findIndex((e) => e.status === "in_progress");
  if (focus < 0) focus = entries.findIndex((e) => e.status !== "completed");
  const at = focus >= 0 ? focus : entries.length;
  const start = Math.max(0, Math.min(at - 1, entries.length - size));
  const end = Math.min(entries.length, start + size);
  return { start, end, before: start, openAfter: entries.slice(end).filter((e) => e.status !== "completed").length, focus };
}

// ---------------------------------------------------------------------------------------------
// The approval card (TC-37 … TC-43)

export type PermitAction = "run" | "read" | "write" | "edit" | "search" | "websearch" | "fetch" | "skill" | "agent" | "use";

export interface PermitView {
  action: PermitAction;
  object: string;
  command?: string;
  description?: string;
  reason?: string;
  /** What an edit would change, for the preview. */
  diff?: { path: string; old: string | null; new: string };
  /** The path of a file object (its tooltip). */
  path?: string;
  /** It may be allowed for the rest of the session. */
  session: boolean;
  /** Runs in the background / outside the sandbox (said under the command). */
  background: boolean;
  unsandboxed: boolean;
}

export function permitView(r: Extract<Request, { kind: "permission" }>): PermitView {
  const input = inputOf(r);
  const base = { reason: r.reason, session: r.suggestions.includes("session"), background: input.run_in_background === true, unsandboxed: input.dangerouslyDisableSandbox === true };
  const fileOf = (action: PermitAction, diff?: PermitView["diff"]): PermitView => {
    const path = str(input.file_path) || str(input.notebook_path) || r.title || "";
    return { ...base, action, object: baseName(path), path, ...(diff ? { diff } : {}) };
  };
  switch (r.tool) {
    case "Bash":
    case "PowerShell": {
      const command = str(input.command) || r.title || "";
      const description = str(input.description).trim();
      return { ...base, action: "run", object: description || command.split("\n")[0], command, ...(description ? { description } : {}) };
    }
    case "Read":
    case "NotebookRead":
      return fileOf("read");
    case "Write":
      return fileOf("write", { path: str(input.file_path), old: null, new: str(input.content) });
    case "Edit":
      return fileOf("edit", { path: str(input.file_path), old: str(input.old_string), new: str(input.new_string) });
    case "MultiEdit": {
      const edits = Array.isArray(input.edits) ? (input.edits as Array<Record<string, unknown>>) : [];
      return fileOf("edit", edits.length ? { path: str(input.file_path), old: edits.map((e) => str(e.old_string)).join("\n…\n"), new: edits.map((e) => str(e.new_string)).join("\n…\n") } : undefined);
    }
    case "NotebookEdit":
      return fileOf("edit");
    case "Grep":
    case "Glob":
      return { ...base, action: "search", object: str(input.pattern) || r.title || "" };
    case "WebSearch":
      return { ...base, action: "websearch", object: str(input.query) || r.title || "" };
    case "WebFetch": {
      const url = str(input.url) || r.title || "";
      let host = url;
      try {
        host = new URL(url).hostname || url;
      } catch {
        // Not a URL: shown as given.
      }
      return { ...base, action: "fetch", object: host };
    }
    case "Skill":
      return { ...base, action: "skill", object: `/${(str(input.skill) || str(input.command) || r.title || "").replace(/^\//, "")}`, description: str(input.args) || undefined };
    case "Task":
    case "Agent":
      return { ...base, action: "agent", object: str(input.description) || r.title || "" };
  }
  return { ...base, action: "use", object: r.tool.startsWith("mcp__") ? mcpLabel(r.tool) : r.tool };
}

/** How long after a card appears (or comes back into view) its keys and clicks do nothing (TC-43). */
export const ARM_MS = 400;
/** After a card is answered and the composer has the focus again, an Enter this soon is the same press (CP-06). */
export const STRAY_ENTER_MS = 500;

export interface CardKey {
  key: string;
  meta: boolean;
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
  repeat: boolean;
  composing: boolean;
  now: number;
  /** Where the focus is: the composer, the card, some other field or menu, or anywhere else. */
  focus: "composer" | "card" | "field" | "other";
}

export interface CardKeyCtx {
  armedAt: number;
  /** The composer has text: ⌘↵ sends it rather than allowing. */
  draft: boolean;
  /** "Allow for this session" is offered. */
  session: boolean;
  /** Only a click may allow (the request asks for that). */
  clickOnly?: boolean;
  /** Buttons in order (deny first), for 1–9. */
  buttons: number;
  /** A menu of the card is open: keys are its. */
  menuOpen?: boolean;
}

export type CardAction = { act: "deny" | "once" | "session" } | { act: "button"; index: number } | { act: "swallow" } | null;

/**
 * What a key does to the approval card at the front: Esc denies, ⌘/Ctrl+↵ allows once (unless the
 * composer has a draft: then it sends that), ⌘/Ctrl+⇧+↵ allows for the session, 1–9 press the
 * buttons when the focus is not in a text field. Nothing while an input method composes, with Alt,
 * in another field or menu; a held key's repeats and anything in the first 400 ms are swallowed.
 */
export function approvalKey(e: CardKey, ctx: CardKeyCtx): CardAction {
  if (e.composing || e.alt || ctx.menuOpen || e.focus === "field") return null;
  const cmd = e.meta || e.ctrl;
  const digit = /^[1-9]$/.test(e.key) && !cmd && !e.shift ? Number(e.key) : 0;
  let act: CardAction = null;
  if (e.key === "Escape" && !cmd) act = { act: "deny" };
  else if (e.key === "Enter" && cmd) {
    if (e.shift) act = ctx.session && !ctx.clickOnly ? { act: "session" } : null;
    else if (!ctx.draft && !ctx.clickOnly) act = { act: "once" };
  } else if (digit && e.focus !== "composer" && !ctx.clickOnly && digit <= ctx.buttons) act = { act: "button", index: digit - 1 };
  if (!act) return null;
  if (e.repeat || e.now - ctx.armedAt < ARM_MS) return { act: "swallow" };
  return act;
}

// ---------------------------------------------------------------------------------------------
// The question card (TC-46 … TC-49)

/** The answer for a part the person skipped, and for "Other" left empty. */
export const NO_PREFERENCE = "[No preference]";
export const OTHER_EMPTY = "[Other, no details given]";

/** Where the person is in a question: the page, what they picked per part, "Other" (null: not chosen). */
export interface QDraft {
  page: number;
  picks: string[][];
  other: Array<string | null>;
  skipped: boolean[];
}

/** The parts of a question block: its own, or (an older host) one part made of its prompt and options. */
export function questionParts(b: QuestionBlock): QuestionItem[] {
  if (b.questions?.length) return b.questions;
  return [{ question: b.prompt, options: b.options.map((label) => ({ label })) }];
}

export function qInit(n: number): QDraft {
  return { page: 0, picks: Array.from({ length: n }, () => []), other: Array.from({ length: n }, () => null), skipped: Array.from({ length: n }, () => false) };
}

const setAt = <T,>(a: T[], i: number, v: T): T[] => a.map((x, k) => (k === i ? v : x));

/** Picking an option: one choice replaces the last, several toggle; "Other" goes when one is picked alone. */
export function qPick(d: QDraft, parts: QuestionItem[], i: number, label: string): QDraft {
  const multi = !!parts[i]?.multi;
  const cur = d.picks[i] ?? [];
  const picks = multi ? (cur.includes(label) ? cur.filter((x) => x !== label) : [...cur, label]) : [label];
  return { ...d, picks: setAt(d.picks, i, picks), other: multi ? d.other : setAt(d.other, i, null), skipped: setAt(d.skipped, i, false) };
}

/** Choosing "Other" (with its text); for a part with one choice it replaces what was picked. */
export function qOther(d: QDraft, parts: QuestionItem[], i: number, text: string | null): QDraft {
  const multi = !!parts[i]?.multi;
  return { ...d, other: setAt(d.other, i, text), picks: multi || text === null ? d.picks : setAt(d.picks, i, []), skipped: setAt(d.skipped, i, false) };
}

export function qAnswered(d: QDraft, i: number): boolean {
  return (d.picks[i]?.length ?? 0) > 0 || d.other[i] !== null || !!d.skipped[i];
}

function firstOpen(d: QDraft, n: number): number {
  for (let i = 0; i < n; i++) if (!qAnswered(d, i)) return i;
  return -1;
}

/** The primary button: on to the next part, or (on the last) submit when every part has an answer. */
export function qNext(d: QDraft, parts: QuestionItem[]): { draft: QDraft; submit: boolean } {
  if (!qAnswered(d, d.page)) return { draft: d, submit: false };
  if (d.page < parts.length - 1) return { draft: { ...d, page: d.page + 1 }, submit: false };
  const open = firstOpen(d, parts.length);
  return open < 0 ? { draft: d, submit: true } : { draft: { ...d, page: open }, submit: false };
}

/** Skip: this part has no answer; on to the next, or from the last to the first one left open. */
export function qSkip(d: QDraft, parts: QuestionItem[]): { draft: QDraft; submit: boolean } {
  const next = { ...d, skipped: setAt(d.skipped, d.page, true), picks: setAt(d.picks, d.page, []), other: setAt(d.other, d.page, null) };
  if (d.page < parts.length - 1) return { draft: { ...next, page: d.page + 1 }, submit: false };
  const open = firstOpen(next, parts.length);
  return open < 0 ? { draft: next, submit: true } : { draft: { ...next, page: open }, submit: false };
}

/** The answers by question: the label picked, several in the options' order joined, "Other"'s text. */
export function qAnswers(d: QDraft, parts: QuestionItem[]): Record<string, string> {
  const out: Record<string, string> = {};
  parts.forEach((p, i) => {
    if (d.skipped[i]) {
      out[p.question] = NO_PREFERENCE;
      return;
    }
    const picked = p.options.map((o) => o.label).filter((l) => d.picks[i]?.includes(l));
    const other = d.other[i];
    if (other !== null) picked.push(other.trim() || OTHER_EMPTY);
    out[p.question] = picked.join(", ");
  });
  return out;
}

// ---------------------------------------------------------------------------------------------
// The status line (CP-43 … CP-47)

export type StatusPhase = "connecting" | "sending" | "starting" | "stopping" | "waiting" | "tool" | "asking" | "thinking" | "thought";

export interface StatusIn {
  chat: Chat;
  connected: boolean;
  /** A message of this device is on its way and no turn shows it yet. */
  sending: boolean;
  /** Stop was pressed and the turn has not ended yet. */
  stopping: boolean;
  now: number;
}

export interface Status {
  /** Shown at all (a turn runs, or something is on its way). */
  on: boolean;
  phase: StatusPhase | null;
  /** For thinking: which of the five wordings (by the turn's seconds); for "thought": how long it took. */
  tier?: number;
  thoughtMs?: number;
  elapsedMs: number;
  tokens: number;
  /** A new turn: the label's anti-flicker timers start over. */
  epoch: number;
}

/** The seconds a thinking label changes at (CP-45). */
export const THINK_TIERS = [15, 30, 45, 60];

/** Which wording thinking has after `secs` seconds of the turn: 0 … 4. */
export function thinkTier(secs: number): number {
  return THINK_TIERS.filter((s) => secs >= s).length;
}

export function statusOf(s: StatusIn): Status {
  const c = s.chat;
  const turn = c.turns[c.turns.length - 1] ?? null;
  const live = turn?.status === "running" ? turn : null;
  const busy = c.state === "running" || c.state === "starting" || c.state === "interrupting";
  const elapsedMs = live?.startedTs ? Math.max(0, s.now - live.startedTs) : 0;
  const tokens = live?.usage?.output ?? 0;
  const base = { elapsedMs, tokens, epoch: live?.n ?? turn?.n ?? 0 };
  if (s.stopping || c.state === "interrupting") return { on: true, phase: "stopping", ...base };
  if (!s.connected && (busy || s.sending)) return { on: true, phase: "connecting", ...base };
  if (c.state === "starting") return { on: true, phase: "starting", ...base };
  if (!busy && !live) return { on: s.sending, phase: s.sending ? "sending" : null, ...base };
  if (!live) return { on: true, phase: s.sending ? "sending" : "waiting", ...base };
  const last = live.blocks[live.blocks.length - 1];
  if (c.requests.some((r) => !r.resolved && r.turn === live.n) || live.blocks.some((b) => b.kind === "question" && !b.answered && !b.outcome)) {
    return { on: true, phase: "asking", ...base };
  }
  // Thinking's words come before the waiting ones (the revised CP-44).
  if (last?.kind === "thought") return { on: true, phase: "thinking", tier: thinkTier(elapsedMs / 1000), ...base };
  const thought = [...live.blocks].reverse().find((b): b is ThoughtBlock => b.kind === "thought");
  if (!last) return { on: true, phase: "waiting", ...base };
  if (last.kind === "tool" && running(last)) return { on: true, phase: "tool", ...base };
  if (thought) return { on: true, phase: "thought", thoughtMs: Math.max(0, thought.endedTs - thought.startedTs), ...base };
  return { on: true, phase: last.kind === "tool" ? "waiting" : null, ...base };
}

/** A label's flicker guard, first layer (CP-47): a new value must hold 150 ms before it replaces the
 *  shown one, except that becoming empty, coming from empty and a new epoch show at once. */
export interface Debounce<T> {
  shown: T | null;
  next: T | null;
  since: number;
  epoch: number;
}

export const DEBOUNCE_MS = 150;
export const DWELL_MS = 650;

export function debounceStep<T>(d: Debounce<T> | null, value: T | null, now: number, epoch = 0, holdEmpty = false): { state: Debounce<T>; wake: number | null } {
  if (!d || d.epoch !== epoch) return { state: { shown: value, next: value, since: now, epoch }, wake: null };
  if (value === d.shown) return { state: { ...d, next: value, since: now }, wake: null };
  // While the turn has said nothing yet, an empty value keeps the last words up.
  if (value === null && holdEmpty) return { state: { ...d, next: d.shown, since: now }, wake: null };
  if (value === null || d.shown === null) return { state: { shown: value, next: value, since: now, epoch }, wake: null };
  const since = value === d.next ? d.since : now;
  if (now - since >= DEBOUNCE_MS) return { state: { shown: value, next: value, since: now, epoch }, wake: null };
  return { state: { ...d, next: value, since }, wake: since + DEBOUNCE_MS };
}

/** Second layer (CP-47, TR-31, TC-12): what is shown stays at least `min` ms; a new epoch resets it. */
export interface Dwell<T> {
  shown: T | null;
  at: number;
  epoch: number;
}

export function dwellStep<T>(d: Dwell<T> | null, value: T | null, now: number, epoch = 0, min = DWELL_MS): { state: Dwell<T>; wake: number | null } {
  if (!d || d.epoch !== epoch) return { state: { shown: value, at: now, epoch }, wake: null };
  if (value === d.shown) return { state: d, wake: null };
  if (now - d.at >= min) return { state: { shown: value, at: now, epoch }, wake: null };
  return { state: d, wake: d.at + min };
}

// ---------------------------------------------------------------------------------------------
// Numbers

/** A token count as the status line says it (CP-46): 950, 12.0k, 1.2M, one decimal kept. */
export function tokenCount(n: number): string {
  const v = Math.max(0, Math.round(n));
  const units: Array<[number, string]> = [[1e9, "B"], [1e6, "M"], [1e3, "k"]];
  for (let i = 0; i < units.length; i++) {
    const [div, u] = units[i];
    if (v >= div) {
      const x = Math.round((v / div) * 10) / 10;
      // 999.96k rounds to 1000.0k: that is 1.0M.
      if (x >= 1000 && i > 0) return `${(Math.round((v / units[i - 1][0]) * 10) / 10).toFixed(1)}${units[i - 1][1]}`;
      return `${x.toFixed(1)}${u}`;
    }
  }
  return String(v);
}

/** The same without a trailing ".0" (the context ring's tooltip, CP-51): 120.5k, 200k. */
export function tokenShort(n: number): string {
  return tokenCount(n).replace(/\.0(?=[kMB]$)/, "");
}

/** A duration as at most three units from the largest that is not zero (TC-13, CP-46). */
export function durationParts(ms: number): Array<{ n: number; unit: "mo" | "w" | "d" | "h" | "m" | "s" }> {
  let s = Math.floor(Math.max(0, ms) / 1000);
  const spans: Array<[number, "mo" | "w" | "d" | "h" | "m" | "s"]> = [[2_592_000, "mo"], [604_800, "w"], [86_400, "d"], [3600, "h"], [60, "m"], [1, "s"]];
  const out: Array<{ n: number; unit: "mo" | "w" | "d" | "h" | "m" | "s" }> = [];
  for (const [len, unit] of spans) {
    const n = Math.floor(s / len);
    s -= n * len;
    if (n || out.length) out.push({ n, unit });
  }
  if (!out.length) return [{ n: 0, unit: "s" }];
  return out.slice(0, 3).filter((p, i) => i === 0 || p.n > 0);
}

/** The context ring's color tier (CP-50). */
export function ringTier(pct: number): "normal" | "warning" | "critical" {
  return pct >= 90 ? "critical" : pct >= 75 ? "warning" : "normal";
}

// ---------------------------------------------------------------------------------------------
// The composer: Claude's permission modes, the queue's preview

/** Claude's permission modes in the menu's order (CP-54). */
export const MODES = ["default", "acceptEdits", "plan", "bypassPermissions"] as const;
export type Mode = (typeof MODES)[number];

/** Choosing a mode: at once, after a confirmation (skipping every permission check), or not while a turn runs. */
export function modeChange(mode: string, busy: boolean): "ok" | "confirm" | "busy" {
  if (mode !== "bypassPermissions") return "ok";
  return busy ? "busy" : "confirm";
}

/** One line for a queued message (CP-27): whitespace folded, "…" when there is nothing. */
export function queuePreview(text: string): string {
  return text.replace(/\s+/g, " ").trim() || "…";
}

// ---------------------------------------------------------------------------------------------
// Pacing a streamed reply (TR-24): words come out in steps of about 22 characters, faster the more
// is waiting, so the text runs about 550 ms behind what arrived; a long first block or a big jump
// is shown at once.

export const PACE = { step: 22, lagMs: 550, minMs: 25, maxMs: 150, joinAbove: 400, rejoinAbove: 600 };

/** Where showing starts: all of a text seen before or a first block over 400 characters, else nothing yet. */
export function paceStart(text: string, seen: boolean): number {
  return seen || text.length > PACE.joinAbove ? text.length : 0;
}

/** The next step from `shown` toward `text`, and how long to wait before it (null: caught up).
 *  `arrived` is how much came in the last update. */
export function paceNext(shown: number, text: string, arrived = 0): { next: number; delay: number | null } {
  const backlog = text.length - shown;
  if (backlog <= 0) return { next: Math.min(shown, text.length), delay: null };
  if (backlog > PACE.rejoinAbove && arrived > PACE.rejoinAbove) return { next: text.length, delay: null };
  let end = Math.min(text.length, shown + PACE.step);
  if (end < text.length) {
    // Cut after the last space of the step when it is past its middle, so words come whole.
    const sp = text.slice(shown, end).lastIndexOf(" ");
    if (sp >= PACE.step / 2) end = shown + sp + 1;
  }
  const delay = Math.min(PACE.maxMs, Math.max(PACE.minMs, (PACE.lagMs * PACE.step) / backlog));
  return { next: end, delay };
}

// ---------------------------------------------------------------------------------------------
// Following the end of the transcript (TR-14, TR-16, TR-17)

/** At the end within this many pixels; a reply starting this close takes the follow back; the jump
 *  button shows past 200 px, the fade past 8 px. */
export const FOLLOW = { atEnd: 2, rejoin: 8, jump: 200, fade: 8 };

/** Keys that move the transcript up (they stop following) or down. */
export function scrollKey(key: string, shift: boolean): "up" | "down" | null {
  if (key === "ArrowUp" || key === "PageUp" || key === "Home" || (key === " " && shift)) return "up";
  if (key === "ArrowDown" || key === "PageDown" || key === "End" || key === " ") return "down";
  return null;
}
