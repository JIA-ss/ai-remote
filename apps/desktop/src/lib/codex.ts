// How a Codex conversation is shown: the way Codex's own desktop app lays out a turn. Pure, so it is
// testable without a page (tools/chat-codex.test.mjs); the components are in ui/chat/codex/.
//
// A turn there is: the user's message on the right; the agent's work as a short list of one-line
// units — consecutive tool calls folded into one line that names what is happening now ("Running
// `npm test`", "Reading app.ts") and, once done, what was done ("Edited files, read files, and ran
// commands"), with the agent's remarks between them; then, once the answer starts, all of that folds
// behind "Worked for 1m 23s", the answer follows, and the files the turn changed close it. This
// module turns the events of a turn (lib/chat.ts) into that structure. The layout was studied from
// Codex's app; the code and wording here are this project's own.

import { baseName, diffRows, type Block, type MessagePhase, type PlanEntry, type QuestionBlock, type TextBlock, type ThoughtBlock, type ToolItem, type Turn } from "./chat";

/** Whether a session's agent is Codex: by its protocol, or (hosts that do not say) its id or name. */
export function isCodexRuntime(runtime: { id?: string; name?: string; protocol?: string | null } | null | undefined): boolean {
  if (!runtime) return false;
  if (runtime.protocol) return runtime.protocol === "codex-app-server";
  return runtime.id === "codex" || /\bcodex\b/i.test(runtime.name ?? "");
}

// ---------------------------------------------------------------------------------------------
// Commands

/** What a command does, as Codex reads it: reads a file, lists files, searches, or something else. */
export type CmdAction =
  | { type: "read"; path: string; name: string }
  | { type: "list_files"; path: string | null }
  | { type: "search"; query: string | null; path: string | null }
  | { type: "unknown" };

export interface ExecInfo {
  /** The command as typed, without the shell Codex runs it in. */
  command: string;
  cwd: string | null;
  actions: CmdAction[];
  /** Only reads, searches and listings: shown as "Read app.ts", not as a command. */
  explore: boolean;
  exitCode: number | null;
  durationMs: number | null;
  /** The output, without the "(exit code N)" line older hosts add. */
  output: string;
}

/** Splits a command line into words the way a POSIX shell quotes them. */
export function splitWords(s: string): string[] {
  const out: string[] = [];
  let cur = "";
  let started = false;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c === "'") {
      started = true;
      const end = s.indexOf("'", i + 1);
      cur += s.slice(i + 1, end < 0 ? s.length : end);
      i = end < 0 ? s.length : end;
    } else if (c === '"') {
      started = true;
      i++;
      for (; i < s.length && s[i] !== '"'; i++) {
        if (s[i] === "\\" && /["\\$`]/.test(s[i + 1] ?? "")) i++;
        cur += s[i];
      }
    } else if (c === "\\" && i + 1 < s.length && /[\s'"\\]/.test(s[i + 1])) {
      started = true;
      cur += s[++i];
    } else if (/\s/.test(c)) {
      if (started) out.push(cur);
      cur = "";
      started = false;
    } else {
      started = true;
      cur += c;
    }
  }
  if (started) out.push(cur);
  return out;
}

const program = (w: string) => w.split(/[\\/]/).at(-1)!.toLowerCase().replace(/\.exe$/, "");

/** A command without the shell around it (`/bin/zsh -lc 'ls -la'` → `ls -la`); hosts send it unwrapped. */
export function unwrapShell(raw: string): string {
  const words = splitWords(raw.trim());
  if (!words.length) return raw;
  const p = program(words[0]);
  if (["bash", "zsh", "sh", "dash", "fish", "ksh"].includes(p)) {
    let at = 1;
    while (at < words.length && words[at].startsWith("-")) at++;
    if (words.slice(1, at).some((f) => f.includes("c")) && words.length === at + 1) return words[at].trim();
  } else if (p === "powershell" || p === "pwsh") {
    const at = words.findIndex((w) => /^-(command|c)$/i.test(w));
    if (at > 0 && at + 1 < words.length) return words.slice(at + 1).join(" ").trim();
  } else if (p === "cmd") {
    const at = words.findIndex((w) => /^\/c$/i.test(w));
    if (at > 0 && at + 1 < words.length) return words.slice(at + 1).join(" ").trim();
  }
  return raw.trim();
}

/** Splits a command line at `&&`, `||`, `;` and `|` outside quotes; `|` ends a segment's own command. */
function segments(cmd: string): string[][] {
  const out: string[][] = [[]];
  let cur = "";
  let quote: string | null = null;
  const push = (newSegment: boolean) => {
    out[out.length - 1].push(cur.trim());
    cur = "";
    if (newSegment) out.push([]);
  };
  for (let i = 0; i < cmd.length; i++) {
    const c = cmd[i];
    if (quote) {
      cur += c;
      if (c === "\\" && quote === '"' && i + 1 < cmd.length) cur += cmd[++i];
      else if (c === quote) quote = null;
      continue;
    }
    if (c === "'" || c === '"') {
      quote = c;
      cur += c;
    } else if ((c === "&" && cmd[i + 1] === "&") || (c === "|" && cmd[i + 1] === "|")) {
      push(true);
      i++;
    } else if (c === ";" || c === "\n") {
      push(true);
    } else if (c === "|") {
      push(false);
    } else {
      cur += c;
    }
  }
  push(false);
  return out.map((s) => s.filter(Boolean)).filter((s) => s.length);
}

/** Options of search tools that take a value (so the value is not the pattern). */
const SEARCH_VALUE_FLAGS = new Set(["-g", "--glob", "-t", "--type", "-T", "--type-not", "-m", "--max-count", "-A", "-B", "-C",
  "--context", "--after-context", "--before-context", "-f", "--file", "--max-depth", "-d", "--include", "--exclude",
  "--exclude-dir", "--color", "--colors", "-j", "--threads", "-M", "--max-columns", "--sort", "--sortr", "--type-add"]);

function positional(words: string[], valueFlags: Set<string> = new Set()): string[] {
  const out: string[] = [];
  for (let i = 1; i < words.length; i++) {
    const w = words[i];
    if (w === "--") {
      out.push(...words.slice(i + 1));
      break;
    }
    if (w.startsWith("-") && w.length > 1) {
      if (valueFlags.has(w)) i++;
      continue;
    }
    out.push(w);
  }
  return out;
}

/** What one command does (the first command of a pipeline decides). */
function actionOf(words: string[]): CmdAction[] | "noop" {
  const one = actionOfOne(words);
  if (one === "noop" || one.type !== "read") return one === "noop" ? one : [one];
  // `cat a b` reads both.
  const p = program(words[0] ?? "");
  if (["cat", "bat", "batcat", "less", "more", "nl"].includes(p)) {
    const files = positional(words);
    if (files.length > 1) return files.map((f) => ({ type: "read", path: f, name: baseName(f) }));
  }
  return [one];
}

function actionOfOne(words: string[]): CmdAction | "noop" {
  const p = program(words[0] ?? "");
  const args = positional(words);
  const last = args.at(-1) ?? null;
  switch (p) {
    case "echo":
    case "printf":
    case "true":
    case "pwd":
    case ":":
      return "noop";
    case "cd":
      return "noop";
    case "cat":
    case "bat":
    case "batcat":
    case "less":
    case "more":
    case "nl":
    case "head":
    case "tail":
    case "get-content":
    case "type":
      if (p === "head" || p === "tail") {
        const files = positional(words, new Set(["-n", "-c", "--lines", "--bytes"]));
        return files.length ? { type: "read", path: files.at(-1)!, name: baseName(files.at(-1)!) } : { type: "unknown" };
      }
      return last ? { type: "read", path: last, name: baseName(last) } : { type: "unknown" };
    case "sed": {
      // `sed -n '1,80p' file` prints part of a file.
      if (!words.includes("-n") || args.length < 2 || !/^\d*,?\d*p$/.test(args[0])) return { type: "unknown" };
      return { type: "read", path: last!, name: baseName(last!) };
    }
    case "rg":
    case "grep":
    case "egrep":
    case "fgrep":
    case "ag":
    case "ack":
    case "select-string": {
      if (words.includes("--files")) return { type: "list_files", path: positional(words, SEARCH_VALUE_FLAGS)[0] ?? null };
      const e = words.findIndex((w) => w === "-e" || w === "--regexp");
      const pos = positional(words, SEARCH_VALUE_FLAGS);
      if (e > 0) return { type: "search", query: words[e + 1] ?? null, path: pos.find((w) => w !== words[e + 1]) ?? null };
      return { type: "search", query: pos[0] ?? null, path: pos[1] ?? null };
    }
    case "git":
      if (words[1] === "grep") {
        const pos = positional(words.slice(1), SEARCH_VALUE_FLAGS);
        return { type: "search", query: pos[0] ?? null, path: pos[1] ?? null };
      }
      if (words[1] === "ls-files") return { type: "list_files", path: positional(words.slice(1))[0] ?? null };
      return { type: "unknown" };
    case "fd":
    case "fdfind": {
      const pos = positional(words, SEARCH_VALUE_FLAGS);
      return pos.length ? { type: "search", query: pos[0], path: pos[1] ?? null } : { type: "list_files", path: null };
    }
    case "find": {
      const name = words.findIndex((w) => w === "-name" || w === "-iname" || w === "-path");
      const root = words[1] && !words[1].startsWith("-") ? words[1] : null;
      return name > 0 ? { type: "search", query: words[name + 1] ?? null, path: root } : { type: "list_files", path: root };
    }
    case "ls":
    case "tree":
    case "eza":
    case "exa":
    case "dir":
    case "get-childitem":
    case "gci":
      return { type: "list_files", path: last };
    default:
      return { type: "unknown" };
  }
}

/** Codex's reading of a command where the host did not send one: reads, searches and listings, else unknown. */
export function parseCommand(cmd: string): CmdAction[] {
  const out: CmdAction[] = [];
  for (const seg of segments(cmd)) {
    const a = actionOf(splitWords(seg[0]));
    if (a === "noop") continue;
    out.push(...a);
  }
  if (!out.length || out.some((a) => a.type === "unknown")) return [{ type: "unknown" }];
  return out;
}

function hostActions(v: unknown): CmdAction[] | null {
  if (!Array.isArray(v)) return null;
  return v.map((a: Record<string, unknown>) => {
    const s = (k: string) => (typeof a?.[k] === "string" && a[k] ? (a[k] as string) : null);
    switch (a?.type) {
      case "read":
        return { type: "read", path: s("path") ?? s("name") ?? "", name: s("name") ?? baseName(s("path") ?? "") };
      case "listFiles":
      case "list_files":
        return { type: "list_files", path: s("path") };
      case "search":
        return { type: "search", query: s("query"), path: s("path") };
      default:
        return { type: "unknown" };
    }
  });
}

const EXIT_LINE = /\n?\(exit code (-?\d+)\)\s*$/;

// A tool call's object is replaced whenever it changes (lib/chat.ts copies on write), so what is
// read from one is kept per object: a running turn is projected again on every event, and its
// finished calls (with their diffs) are not parsed again each time.
const execCache = new WeakMap<ToolItem, ExecInfo>();
const filesCache = new WeakMap<ToolItem, FileChange[]>();

/** A shell tool call as Codex shows it: the command, what it does, how it ended. */
export function execInfo(x: ToolItem): ExecInfo {
  let v = execCache.get(x);
  if (!v) execCache.set(x, (v = readExec(x)));
  return v;
}

function readExec(x: ToolItem): ExecInfo {
  const input = (x.input ?? {}) as { command?: unknown; cwd?: unknown; actions?: unknown };
  const fromTitle = x.title.replace(/^Shell\s+/, "");
  const command = typeof input.command === "string" ? input.command : unwrapShell(fromTitle);
  let actions = hostActions(input.actions) ?? parseCommand(command);
  if (!actions.length) actions = [{ type: "unknown" }];
  const explore = actions.every((a) => a.type !== "unknown");
  // Codex streams a command's output in pieces (each is the next bytes, not a line of its own).
  let output = x.content.filter((c): c is { type: "text"; text: string } => c.type === "text").map((c) => c.text).join("");
  let exitCode = typeof x.exitCode === "number" ? x.exitCode : null;
  const m = EXIT_LINE.exec(output);
  if (m) {
    output = output.slice(0, m.index);
    exitCode ??= Number(m[1]);
  }
  if (exitCode === null && x.status === "completed" && !x.stopped) exitCode = 0;
  const durationMs = typeof x.durationMs === "number" ? x.durationMs : x.endedTs !== null ? x.endedTs - x.startedTs : null;
  return { command, cwd: typeof input.cwd === "string" ? input.cwd : null, actions, explore, exitCode, durationMs, output };
}

/** A path as Codex shows it: relative to the folder the agent works in when it is inside it. */
export function relPath(path: string, cwd: string | null | undefined): string {
  if (!cwd || !path) return path;
  const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "");
  const p = norm(path);
  const base = norm(cwd);
  if (p === base) return ".";
  if (p.toLowerCase().startsWith(`${base.toLowerCase()}/`)) return p.slice(base.length + 1);
  return path;
}

// ---------------------------------------------------------------------------------------------
// File changes

export type DiffLine =
  | { kind: "hunk"; text: string }
  | { kind: "context" | "add" | "del"; text: string; old: number | null; new: number | null };

export interface FileChange {
  path: string;
  movePath: string | null;
  kind: "add" | "delete" | "update";
  add: number;
  del: number;
  lines: DiffLine[];
}

/** The lines of a unified diff with their line numbers (a new file's "diff" may be its content). */
export function parseUnifiedDiff(diff: string, kind: FileChange["kind"] = "update"): { lines: DiffLine[]; add: number; del: number } {
  const raw = diff.replace(/\r\n/g, "\n").split("\n");
  if (raw.at(-1) === "") raw.pop();
  const lines: DiffLine[] = [];
  let add = 0;
  let del = 0;
  if (!raw.some((l) => l.startsWith("@@"))) {
    // No hunks: an added file comes as its content, a deleted one as what it held.
    raw.forEach((text, i) => {
      if (kind === "delete") {
        lines.push({ kind: "del", text, old: i + 1, new: null });
        del++;
      } else if (kind === "add") {
        lines.push({ kind: "add", text, old: null, new: i + 1 });
        add++;
      }
    });
    return { lines, add, del };
  }
  let o = 0;
  let n = 0;
  let inHunk = false;
  for (const l of raw) {
    const h = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)$/.exec(l);
    if (h) {
      o = Number(h[1]);
      n = Number(h[2]);
      inHunk = true;
      lines.push({ kind: "hunk", text: l });
      continue;
    }
    if (!inHunk || l.startsWith("\\")) continue;
    if (l.startsWith("+")) {
      lines.push({ kind: "add", text: l.slice(1), old: null, new: n++ });
      add++;
    } else if (l.startsWith("-")) {
      lines.push({ kind: "del", text: l.slice(1), old: o++, new: null });
      del++;
    } else {
      lines.push({ kind: "context", text: l.slice(1), old: o++, new: n++ });
    }
  }
  return { lines, add, del };
}

/** The files an edit changed, one entry per file (hosts that send only per-hunk diffs are merged per path). */
export function fileChanges(x: ToolItem): FileChange[] {
  let v = filesCache.get(x);
  if (!v) filesCache.set(x, (v = readFiles(x)));
  return v;
}

function readFiles(x: ToolItem): FileChange[] {
  const files = (x.input as { files?: unknown } | undefined)?.files;
  if (Array.isArray(files) && files.length) {
    return files.map((f: Record<string, unknown>) => {
      const kind = f.kind === "add" || f.kind === "delete" ? f.kind : "update";
      const parsed = parseUnifiedDiff(typeof f.diff === "string" ? f.diff : "", kind);
      return { path: String(f.path ?? ""), movePath: typeof f.move_path === "string" ? f.move_path : null, kind, ...parsed };
    });
  }
  const out: FileChange[] = [];
  for (const c of x.content) {
    if (c.type !== "diff") continue;
    let f = out.find((e) => e.path === c.path);
    if (!f) {
      f = { path: c.path, movePath: null, kind: c.old === null ? "add" : c.new === "" ? "delete" : "update", add: 0, del: 0, lines: [] };
      out.push(f);
    } else if (f.lines.length) {
      f.lines.push({ kind: "hunk", text: "" });
    }
    // A text's last newline is not a line of its own.
    const trim = (t: string) => (t.endsWith("\n") ? t.slice(0, -1) : t);
    for (const r of diffRows(c.old === null ? null : trim(c.old), trim(c.new))) {
      if (r.kind === "gap") f.lines.push({ kind: "hunk", text: "" });
      else {
        f.lines.push({ kind: r.kind, text: r.text, old: null, new: null });
        if (r.kind === "add") f.add++;
        else if (r.kind === "del") f.del++;
      }
    }
  }
  if (!out.length) {
    for (const p of x.locations) out.push({ path: p, movePath: null, kind: "update", add: 0, del: 0, lines: [] });
  }
  return out;
}

/** The files a whole turn changed, merged per path in the order first changed. */
export function turnChanges(turn: Turn): FileChange[] {
  const out = new Map<string, FileChange>();
  for (const b of turn.blocks) {
    if (b.kind !== "tool" || !isEditTool(b) || b.status === "failed") continue;
    for (const f of fileChanges(b)) {
      const have = out.get(f.path);
      if (!have) {
        out.set(f.path, { ...f, lines: f.lines.slice() });
        continue;
      }
      have.add += f.add;
      have.del += f.del;
      have.lines.push(...f.lines);
      have.movePath = f.movePath ?? have.movePath;
      if (f.kind === "delete") have.kind = "delete";
    }
  }
  return [...out.values()];
}

export function isEditTool(x: ToolItem): boolean {
  return x.tool === "edit" || x.tool === "delete" || x.tool === "move" || x.content.some((c) => c.type === "diff");
}

// ---------------------------------------------------------------------------------------------
// The units of a turn

/** One tool call of the agent's work. */
export type Act =
  | { kind: "exec"; key: string; tool: ToolItem; info: ExecInfo }
  | { kind: "patch"; key: string; tool: ToolItem; files: FileChange[] }
  | { kind: "web"; key: string; tool: ToolItem; query: string }
  | { kind: "tool"; key: string; tool: ToolItem }
  /** Reasoning between two tool calls of a group. */
  | { kind: "thought"; key: string; block: ThoughtBlock };

export type Unit =
  /** Tool calls in a row, shown as one line. */
  | { kind: "group"; key: string; acts: Act[]; live: boolean }
  /** What the agent said between steps. */
  | { kind: "message"; key: string; block: TextBlock; streaming: boolean }
  | { kind: "thought"; key: string; block: ThoughtBlock; live: boolean }
  /** A sub-agent the agent started. */
  | { kind: "agent"; key: string; tool: ToolItem }
  | { kind: "question"; key: string; block: QuestionBlock }
  | { kind: "notice"; key: string; level: string; code: string; text: string }
  /** A message the user added to the running turn. */
  | { kind: "steer"; key: string; block: Extract<Block, { kind: "user" }> };

export interface CodexTurn {
  units: Unit[];
  /** The agent's answer to the turn, shown after the work. */
  answer: { block: TextBlock; streaming: boolean } | null;
  /** The work folds behind "Worked for …" (the answer started and the turn was not stopped). */
  foldable: boolean;
  status: "working" | "worked" | "stopped" | "failed";
  /** The agent is thinking now (nothing else shows that it is working). */
  thinking: boolean;
  /** A question waits for the user's answer. */
  asking: boolean;
  /** The turn's plan as last updated (shown above its work, not as a step of it). */
  plan: PlanEntry[] | null;
  changes: FileChange[];
}

const running = (x: ToolItem) => x.status === "in_progress" || x.status === "pending";

/** A sub-agent call (Codex's collaboration tools). */
export function isAgentTool(x: ToolItem): boolean {
  return x.tool === "think" && /^(agent|Task|Agent)\b/.test(x.title);
}

function actOf(x: ToolItem): Act {
  const key = `k:${x.callId}`;
  if (x.tool === "execute") return { kind: "exec", key, tool: x, info: execInfo(x) };
  if (isEditTool(x)) return { kind: "patch", key, tool: x, files: fileChanges(x) };
  if (x.tool === "fetch" && /^search\b/.test(x.title)) {
    const q = (x.input as { query?: unknown } | undefined)?.query;
    return { kind: "web", key, tool: x, query: typeof q === "string" ? q : x.title.replace(/^search\s*/, "") };
  }
  return { kind: "tool", key, tool: x };
}

/** The block that is the turn's answer, if it has started. */
function answerIndex(turn: Turn): number {
  const b = turn.blocks;
  let last = b.length - 1;
  // Reasoning, notices and a plan after the answer do not make it less of an answer.
  while (last >= 0 && (b[last].kind === "thought" || b[last].kind === "notice" || b[last].kind === "plan")) last--;
  const x = b[last];
  if (!x || x.kind !== "text" || !x.text.trim()) return -1;
  if (x.phase === "final_answer") return last;
  if (x.phase === "commentary") return -1;
  // A host that does not say: the last text of a finished turn, after all its work.
  const phased = b.some((y) => y.kind === "text" && y.phase);
  return !phased && turn.status !== "running" ? last : -1;
}

/** The units of a turn, its answer, and how its work is summed up. */
export function codexTurn(turn: Turn): CodexTurn {
  const live = turn.status === "running";
  const b = turn.blocks;
  const ans = answerIndex(turn);
  const units: Unit[] = [];
  let group: Extract<Unit, { kind: "group" }> | null = null;
  let plan: PlanEntry[] | null = null;
  const groupable = (x: Block | undefined) => x?.kind === "tool" && !isAgentTool(x);
  for (let i = 0; i < b.length; i++) {
    if (i === ans) continue;
    const x = b[i];
    if (x.kind === "plan") {
      // A plan update is not a step: it neither shows in the work nor splits a group.
      plan = x.entries;
      continue;
    }
    if (groupable(x)) {
      const tool = x as ToolItem;
      if (!group) {
        group = { kind: "group", key: `g${turn.n}:${tool.callId}`, acts: [], live: false };
        units.push(group);
      }
      group.acts.push(actOf(tool));
      continue;
    }
    if (x.kind === "thought") {
      // Reasoning between two tool calls stays in their group; anywhere else it is a line of its own.
      let next = i + 1;
      while (next < b.length && (b[next].kind === "thought" || b[next].kind === "plan")) next++;
      if (group && groupable(b[next])) {
        group.acts.push({ kind: "thought", key: `t:${x.id}`, block: x });
        continue;
      }
      group = null;
      if (x.text.trim()) units.push({ kind: "thought", key: `t${turn.n}:${x.id}`, block: x, live: false });
      continue;
    }
    group = null;
    switch (x.kind) {
      case "text":
        if (x.text.trim()) units.push({ kind: "message", key: `m${turn.n}:${x.id}`, block: x, streaming: false });
        break;
      case "tool":
        units.push({ kind: "agent", key: `a${turn.n}:${x.callId}`, tool: x });
        break;
      case "question":
        units.push({ kind: "question", key: `q${turn.n}:${x.qid}`, block: x });
        break;
      case "notice":
        units.push({ kind: "notice", key: `n${turn.n}:${i}`, level: x.level, code: x.code, text: x.text });
        break;
      case "user":
        units.push({ kind: "steer", key: `s${turn.n}:${i}`, block: x });
        break;
    }
  }
  const lastBlock = b[b.length - 1];
  const lastUnit = units[units.length - 1];
  const asking = live && b.some((y) => y.kind === "question" && !y.answered);
  let thinking = false;
  if (live) {
    for (const u of units) {
      if (u.kind === "group") u.live = u.acts.some((a) => a.kind !== "thought" && running(a.tool));
    }
    if (lastUnit?.kind === "group" && ans < 0) lastUnit.live = true;
    if (lastUnit?.kind === "message" && ans < 0 && lastBlock?.kind === "text") lastUnit.streaming = true;
    if (lastUnit?.kind === "thought" && lastBlock?.kind === "thought") lastUnit.live = true;
    // Nothing on screen says the agent is busy: it is thinking.
    const busy = units.some((u) => (u.kind === "group" && u.live) || (u.kind === "thought" && u.live) || (u.kind === "message" && u.streaming))
      || (ans >= 0 && lastBlock === b[ans]);
    thinking = !busy && !asking;
  }
  const answer = ans >= 0 ? { block: b[ans] as TextBlock, streaming: live && lastBlock === b[ans] } : null;
  const status = live ? "working" : turn.status === "interrupted" ? "stopped" : turn.status === "failed" ? "failed" : "worked";
  return {
    units,
    answer,
    // Reasoning alone is not work worth folding away.
    foldable: answer !== null && turn.status !== "interrupted" && units.some((u) => u.kind !== "thought"),
    status,
    thinking,
    asking,
    plan,
    changes: live ? [] : turnChanges(turn),
  };
}

/** What a group did, by kind, in the order Codex names them: edits, reads, commands, web searches, tools. */
export type SummaryPart = "edited" | "read" | "ran" | "web" | "tools" | "agents";

export function groupSummary(acts: Act[]): { parts: Array<{ part: SummaryPart; n: number }>; add: number; del: number } {
  const edited = new Set<string>();
  const read = new Set<string>();
  let explored = 0;
  let ran = 0;
  let web = 0;
  let tools = 0;
  let add = 0;
  let del = 0;
  for (const a of acts) {
    if (a.kind === "patch") {
      if (a.tool.status === "failed") continue;
      for (const f of a.files) {
        edited.add(f.path);
        add += f.add;
        del += f.del;
      }
    } else if (a.kind === "exec") {
      if (a.info.explore) {
        explored++;
        for (const x of a.info.actions) if (x.type === "read") read.add(x.path);
      } else ran++;
    } else if (a.kind === "web") web++;
    else if (a.kind === "tool") tools++;
  }
  const parts: Array<{ part: SummaryPart; n: number }> = [];
  if (edited.size) parts.push({ part: "edited", n: edited.size });
  if (explored) parts.push({ part: "read", n: Math.max(read.size, 1) });
  if (ran) parts.push({ part: "ran", n: ran });
  if (web) parts.push({ part: "web", n: web });
  if (tools) parts.push({ part: "tools", n: tools });
  return { parts, add, del };
}

/** The act a live group's line names: the one running now, else the last one. */
export function activeAct(acts: Act[]): Act | null {
  for (let i = acts.length - 1; i >= 0; i--) {
    const a = acts[i];
    if (a.kind !== "thought" && running(a.tool)) return a;
  }
  for (let i = acts.length - 1; i >= 0; i--) if (acts[i].kind !== "thought") return acts[i];
  return null;
}

/** How long a turn worked, from its start to its end (or now). */
export function workedMs(turn: Turn, now: number): number {
  if (turn.durationMs !== null && turn.status !== "running") return turn.durationMs;
  const start = turn.startedTs ?? turn.user?.ts ?? now;
  return Math.max(0, now - start);
}

/** "8s", "1m 23s", "2h 5m" (Codex's way), or the Chinese equivalent. */
export function duration(ms: number, zh = false): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return zh ? `${s} 秒` : `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return zh ? `${m} 分 ${s % 60} 秒` : `${m}m ${s % 60}s`;
  return zh ? `${Math.floor(m / 60)} 小时 ${m % 60} 分` : `${Math.floor(m / 60)}h ${m % 60}m`;
}

/** A command's run time for its line ("in 2.4s"): under a second it is not worth saying. */
export function elapsed(ms: number | null, zh = false): string | null {
  if (ms === null || ms < 1000) return null;
  if (ms < 10_000) return zh ? `${(ms / 1000).toFixed(1)} 秒` : `${(ms / 1000).toFixed(1)}s`;
  return duration(ms, zh);
}

/** The step of a plan the agent is on, for the plan's line ("Step 2 / 5"). */
export function planStep(entries: PlanEntry[]): { step: number; total: number; done: number; current: PlanEntry | null } {
  const done = entries.filter((e) => e.status === "completed").length;
  const at = entries.findIndex((e) => e.status === "in_progress");
  const current = at >= 0 ? entries[at] : entries.find((e) => e.status !== "completed") ?? null;
  const step = at >= 0 ? at + 1 : Math.min(done + 1, entries.length);
  return { step, total: entries.length, done, current };
}

export type { MessagePhase };
