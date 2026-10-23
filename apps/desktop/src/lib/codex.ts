// How a Codex conversation is shown: the way Codex's own desktop app lays out a turn. Pure, so it is
// testable without a page (tools/chat-codex.test.mjs); the components are in ui/codex/.
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

/**
 * A command line's commands (split at `&&`, `||`, `;` and newlines outside quotes), each as its pipeline's
 * stages. Null when the line does what a plain reading cannot follow: a redirection (`>`, `<`, `2>&1`),
 * a command substitution, a background job. Such a line shows as a command, never as a read.
 */
function commandList(cmd: string): string[][] | null {
  const out: string[][] = [[]];
  let cur = "";
  let quote: string | null = null;
  const end = (next: boolean) => {
    out[out.length - 1].push(cur.trim());
    cur = "";
    if (next) out.push([]);
  };
  for (let i = 0; i < cmd.length; i++) {
    const c = cmd[i];
    if (quote) {
      if (quote === '"' && (c === "`" || (c === "$" && cmd[i + 1] === "("))) return null;
      cur += c;
      if (c === "\\" && quote === '"' && i + 1 < cmd.length) cur += cmd[++i];
      else if (c === quote) quote = null;
      continue;
    }
    if (c === "'" || c === '"') {
      quote = c;
      cur += c;
    } else if (c === "`" || (c === "$" && cmd[i + 1] === "(") || c === ">" || c === "<") {
      return null;
    } else if ((c === "&" && cmd[i + 1] === "&") || (c === "|" && cmd[i + 1] === "|")) {
      end(true);
      i++;
    } else if (c === "&") {
      return null;
    } else if (c === ";" || c === "\n") {
      end(true);
    } else if (c === "|") {
      end(false);
    } else {
      cur += c;
    }
  }
  end(false);
  return out.map((s) => s.filter(Boolean)).filter((s) => s.length);
}

/** Commands that only reshape what comes down a pipe (and so may follow a read or a search). */
const PIPE_FILTERS = new Set(["head", "tail", "wc", "sort", "uniq", "nl", "cut", "tr", "column", "fold", "fmt", "grep", "egrep", "fgrep",
  "rg", "sed", "jq", "cat", "less", "more", "select-object", "select", "select-string", "sort-object", "measure-object", "format-table",
  "out-string", "ft", "sls"]);

/** Options of search tools that take a value (so the value is not the pattern). */
const SEARCH_VALUE_FLAGS = new Set(["-g", "--glob", "-t", "--type", "-T", "--type-not", "-m", "--max-count", "-A", "-B", "-C",
  "--context", "--after-context", "--before-context", "-f", "--file", "--max-depth", "-d", "--include", "--exclude",
  "--exclude-dir", "--color", "--colors", "-j", "--threads", "-M", "--max-columns", "--sort", "--sortr", "--type-add", "-e", "--regexp"]);

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

/** PowerShell's parameters (`-Path x`, case-insensitive) and the words that are not parameters. */
const PS_SWITCHES = new Set(["-recurse", "-force", "-raw", "-wait", "-casesensitive", "-simplematch", "-list", "-name", "-file",
  "-directory", "-hidden", "-quiet", "-notmatch", "-allmatches", "-asbytestream"]);
function psArgs(words: string[]): { params: Map<string, string | true>; rest: string[] } {
  const params = new Map<string, string | true>();
  const rest: string[] = [];
  for (let i = 1; i < words.length; i++) {
    const w = words[i];
    if (/^-[A-Za-z]/.test(w)) {
      const name = w.toLowerCase();
      if (!PS_SWITCHES.has(name) && i + 1 < words.length && !/^-[A-Za-z]/.test(words[i + 1])) params.set(name, words[++i]);
      else params.set(name, true);
    } else {
      rest.push(w);
    }
  }
  return { params, rest };
}

/** Whether a command writes, deletes or runs something else although its program usually only reads. */
function changesThings(p: string, words: string[]): boolean {
  if (p === "sed") return words.some((w) => /^-[a-zA-Z]*i/.test(w) || w.startsWith("--in-place"));
  if (p === "find") return words.some((w) => ["-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls"].includes(w));
  if (p === "fd" || p === "fdfind") return words.some((w) => ["-x", "-X", "--exec", "--exec-batch"].includes(w));
  if (p === "rg") return words.some((w) => w === "--pre" || w.startsWith("--pre="));
  return false;
}

/** What one command does. */
function actionOf(words: string[]): CmdAction[] | "noop" {
  const p = program(words[0] ?? "");
  if (changesThings(p, words)) return [{ type: "unknown" }];
  const one = actionOfOne(words);
  if (one === "noop" || one.type !== "read") return one === "noop" ? one : [one];
  // `cat a b` reads both.
  if (["cat", "bat", "batcat", "less", "more", "nl"].includes(p)) {
    const files = positional(words);
    if (files.length > 1) return files.map((f) => ({ type: "read", path: f, name: baseName(f) }));
  }
  return [one];
}

const read = (path: string): CmdAction => ({ type: "read", path, name: baseName(path) });

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
    case "cd":
      return "noop";
    case "cat":
    case "bat":
    case "batcat":
    case "less":
    case "more":
    case "nl":
    case "type":
      return last ? read(last) : { type: "unknown" };
    case "head":
    case "tail": {
      const files = positional(words, new Set(["-n", "-c", "--lines", "--bytes"]));
      return files.length ? read(files.at(-1)!) : { type: "unknown" };
    }
    case "get-content":
    case "gc": {
      const { params, rest } = psArgs(words);
      const path = params.get("-path") ?? params.get("-literalpath") ?? rest[0];
      return typeof path === "string" ? read(path) : { type: "unknown" };
    }
    case "sed": {
      // `sed -n '1,80p' file` prints part of a file.
      if (!words.includes("-n") || args.length < 2 || !/^\d*,?\d*p$/.test(args[0])) return { type: "unknown" };
      return read(last!);
    }
    case "rg":
    case "grep":
    case "egrep":
    case "fgrep":
    case "ag":
    case "ack": {
      const pos = positional(words, SEARCH_VALUE_FLAGS);
      if (words.includes("--files")) return { type: "list_files", path: pos[0] ?? null };
      const e = words.findIndex((w) => w === "-e" || w === "--regexp");
      if (e > 0) return { type: "search", query: words[e + 1] ?? null, path: pos[0] ?? null };
      return { type: "search", query: pos[0] ?? null, path: pos[1] ?? null };
    }
    case "select-string":
    case "sls": {
      const { params, rest } = psArgs(words);
      const query = params.get("-pattern") ?? rest[0];
      const path = params.get("-path") ?? params.get("-literalpath") ?? (params.has("-pattern") ? rest[0] : rest[1]);
      return { type: "search", query: typeof query === "string" ? query : null, path: typeof path === "string" ? path : null };
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
    case "get-childitem":
    case "gci":
    case "dir":
    case "ls": {
      if (p === "ls" || p === "dir") return { type: "list_files", path: last };
      const { params, rest } = psArgs(words);
      const path = params.get("-path") ?? params.get("-literalpath") ?? rest[0];
      return { type: "list_files", path: typeof path === "string" ? path : null };
    }
    case "tree":
    case "eza":
    case "exa":
      return { type: "list_files", path: last };
    default:
      return { type: "unknown" };
  }
}

/** Codex's reading of a command where the host did not send one: reads, searches and listings, else unknown. */
export function parseCommand(cmd: string): CmdAction[] {
  const commands = commandList(cmd);
  if (!commands) return [{ type: "unknown" }];
  const out: CmdAction[] = [];
  for (const stages of commands) {
    // What comes after the first command of a pipeline may only reshape its output.
    for (const stage of stages.slice(1)) {
      const w = splitWords(stage);
      const p = program(w[0] ?? "");
      if (!PIPE_FILTERS.has(p) || changesThings(p, w)) return [{ type: "unknown" }];
    }
    const a = actionOf(splitWords(stages[0]));
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
  // Completed without a code: it succeeded, unless it was only closed by the end of its turn.
  if (exitCode === null && x.status === "completed" && !x.stopped && !x.endedByTurn) exitCode = 0;
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
  /** The host cut its diff short (a very large change). */
  truncated?: boolean;
}

/** Whether a "diff" is a unified diff, rather than the content Codex sends for an added or deleted file. */
function looksLikeDiff(diff: string): boolean {
  return /^(@@ -\d|--- |diff --git )/.test(diff.replace(/^\s*\n/, ""));
}

/** The lines of a unified diff with their line numbers (a new file's "diff" may be its content). */
export function parseUnifiedDiff(diff: string, kind: FileChange["kind"] = "update"): { lines: DiffLine[]; add: number; del: number } {
  const raw = diff.replace(/\r\n/g, "\n").split("\n");
  if (raw.at(-1) === "") raw.pop();
  const lines: DiffLine[] = [];
  let add = 0;
  let del = 0;
  if (kind !== "update" ? !looksLikeDiff(diff) : !raw.some((l) => /^@@ -\d/.test(l))) {
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
    const fromContent = contentFiles(x);
    return files.map((f: Record<string, unknown>) => {
      const kind = f.kind === "add" || f.kind === "delete" ? f.kind : "update";
      const path = String(f.path ?? "");
      const movePath = typeof f.move_path === "string" ? f.move_path : null;
      const diff = typeof f.diff === "string" ? f.diff : "";
      // Past the host's budget a file comes without its diff: what the call's content has of it.
      const fallback = !diff && f.truncated ? fromContent.find((c) => c.path === path) : undefined;
      if (fallback) return { ...fallback, kind, movePath, truncated: true };
      return { path, movePath, kind, ...parseUnifiedDiff(diff, kind), ...(f.truncated ? { truncated: true } : {}) };
    });
  }
  return contentFiles(x);
}

/** The files of an edit from its per-hunk diffs (older hosts), merged per path, without line numbers. */
function contentFiles(x: ToolItem): FileChange[] {
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

/** A path joined to the folder it is relative to (a path that is absolute already stays as it is). */
export function joinPath(cwd: string | null | undefined, rel: string): string {
  if (!cwd || /^([A-Za-z]:[\\/]|[\\/])/.test(rel)) return rel;
  const sep = cwd.includes("\\") && !cwd.includes("/") ? "\\" : "/";
  return `${cwd.replace(/[\\/]+$/, "")}${sep}${sep === "\\" ? rel.replace(/\//g, "\\") : rel}`;
}

/** The files of a whole-turn diff (`diff --git a/x b/x` sections, as git and Codex write them). */
export function parseTurnDiff(text: string, cwd?: string | null): FileChange[] {
  const out: FileChange[] = [];
  const sections = text.replace(/\r\n/g, "\n").split(/^(?=diff --git )/m).filter((s) => s.startsWith("diff --git "));
  for (const sec of sections) {
    const lines = sec.split("\n");
    const head = /^diff --git a\/(.+?) b\/(.+)$/.exec(lines[0]);
    let from = head?.[1] ?? null;
    let to = head?.[2] ?? null;
    let kind: FileChange["kind"] = "update";
    const at = lines.findIndex((l) => l.startsWith("@@"));
    for (const l of lines.slice(1, at < 0 ? lines.length : at)) {
      if (l.startsWith("new file mode")) kind = "add";
      else if (l.startsWith("deleted file mode")) kind = "delete";
      else if (l.startsWith("rename from ")) from = l.slice(12);
      else if (l.startsWith("rename to ")) to = l.slice(10);
      else if (l === "--- /dev/null") kind = "add";
      else if (l === "+++ /dev/null") kind = "delete";
    }
    if (!from && !to) continue;
    const body = at < 0 ? "" : lines.slice(at).join("\n");
    const parsed = body ? parseUnifiedDiff(body, "update") : { lines: [], add: 0, del: 0 };
    const path = joinPath(cwd, (kind === "add" ? to : from) ?? to ?? "");
    const moved = kind === "update" && from && to && from !== to ? joinPath(cwd, to) : null;
    out.push({ path, movePath: moved, kind, ...parsed });
  }
  return out;
}

/**
 * The files a whole turn changed: Codex's own diff of the turn where the host passes it on (each file
 * once, as its net change), otherwise each edit's files merged per path in the order first changed.
 */
export function turnChanges(turn: Turn, cwd?: string | null): FileChange[] {
  if (turn.diff && !turn.diff.truncated) {
    const fromDiff = parseTurnDiff(turn.diff.text, cwd);
    if (fromDiff.length) return fromDiff;
  }
  const out = new Map<string, FileChange>();
  for (const b of turn.blocks) {
    if (b.kind !== "tool" || !isEditTool(b) || b.status === "failed") continue;
    for (const f of fileChanges(b)) {
      const have = out.get(f.path);
      if (!have) {
        out.set(f.path, { ...f, lines: f.lines.slice() });
        continue;
      }
      // Created then deleted in the same turn: nothing is left of it.
      if (have.kind === "add" && f.kind === "delete") {
        out.delete(f.path);
        continue;
      }
      have.add += f.add;
      have.del += f.del;
      have.lines.push(...f.lines);
      have.movePath = f.movePath ?? have.movePath;
      // Deleted then written again: it changed; deleted at the end: it is gone. An added file stays added.
      have.kind = have.kind === "delete" && f.kind === "add" ? "update" : f.kind === "delete" ? "delete" : have.kind;
      if (f.truncated) have.truncated = true;
    }
  }
  return [...out.values()];
}

/**
 * A file's change as a patch git applies (`git apply`), or null when it cannot be one: an older host's
 * diffs carry no line numbers.
 */
export function filePatch(f: FileChange, cwd: string | null | undefined): string | null {
  const changed = f.lines.filter((l): l is Exclude<DiffLine, { kind: "hunk" }> => l.kind !== "hunk");
  if (!changed.length || f.truncated || changed.some((l) => l.old === null && l.new === null)) return null;
  const rel = (p: string) => relPath(p, cwd).replace(/\\/g, "/").replace(/^\/+/, "");
  const a = rel(f.path);
  const b = rel(f.movePath ?? f.path);
  const head = [`diff --git a/${a} b/${b}`];
  if (f.kind === "add") head.push("new file mode 100644", "--- /dev/null", `+++ b/${b}`);
  else if (f.kind === "delete") head.push("deleted file mode 100644", `--- a/${a}`, "+++ /dev/null");
  else head.push(`--- a/${a}`, `+++ b/${b}`);
  const body: string[] = [];
  // The content of an added or deleted file comes without a hunk header of its own.
  if (!f.lines.some((l) => l.kind === "hunk" && /^@@ -\d/.test(l.text))) {
    body.push(f.kind === "delete" ? `@@ -1,${changed.length} +0,0 @@` : `@@ -0,0 +1,${changed.length} @@`);
  }
  for (const l of f.lines) {
    if (l.kind === "hunk") body.push(l.text);
    else body.push(`${l.kind === "add" ? "+" : l.kind === "del" ? "-" : " "}${l.text}`);
  }
  return `${[...head, ...body].join("\n")}\n`;
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
  // A host that does not say: the last text of a turn that completed, after all its work (a turn that
  // failed or was stopped has no answer, only what it said on the way).
  const phased = b.some((y) => y.kind === "text" && y.phase);
  return !phased && turn.status === "completed" ? last : -1;
}

/** The units of a turn, its answer, and how its work is summed up (`cwd`: what the turn's diff is relative to). */
export function codexTurn(turn: Turn, cwd?: string | null): CodexTurn {
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
      // Reasoning between two tool calls stays in their group, and so does reasoning after the last one
      // while the turn runs (the next call may follow; the group's line must not come apart meanwhile).
      // Anywhere else it is a line of its own.
      let next = i + 1;
      while (next < b.length && (b[next].kind === "thought" || b[next].kind === "plan")) next++;
      if (group && (groupable(b[next]) || (live && next >= b.length))) {
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
    const busy = units.some((u) => (u.kind === "group" && u.live) || (u.kind === "thought" && u.live) || (u.kind === "message" && u.streaming)
      || (u.kind === "agent" && running(u.tool)))
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
    changes: live ? [] : turnChanges(turn, cwd),
  };
}

/** What a group did, by kind, in the order Codex names them: edits, reads, commands, web searches, tools. */
export type SummaryPart = "edited" | "read" | "ran" | "web" | "tools" | "failed";

export function groupSummary(acts: Act[]): { parts: Array<{ part: SummaryPart; n: number }>; add: number; del: number } {
  const edited = new Set<string>();
  const failed = new Set<string>();
  const read = new Set<string>();
  let explored = 0;
  let ran = 0;
  let web = 0;
  let tools = 0;
  let add = 0;
  let del = 0;
  for (const a of acts) {
    if (a.kind === "patch") {
      if (a.tool.status === "failed" && !a.tool.stopped) {
        for (const f of a.files) failed.add(f.path);
        continue;
      }
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
  // Edits that did not apply are said last, so a group of only those still has words.
  if (failed.size) parts.push({ part: "failed", n: failed.size });
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
