// Coding-agent chats on a host (Claude Code, Codex, ACP agents): the host's events and how they fold
// into what the chat panel shows. Pure, so the folding is testable without a page
// (tools/chat-state.test.mjs). The transport is in api.ts (`openChat`).

export type ChatState = "starting" | "idle" | "running" | "interrupting" | "parked" | "ended";
export type ToolKind = "read" | "edit" | "delete" | "move" | "search" | "execute" | "think" | "fetch" | "other";
export type ToolStatus = "pending" | "in_progress" | "completed" | "failed";
export type TurnStatus = "completed" | "failed" | "interrupted";

/** An agent installed on a host, as `chat_runtimes` reports it. */
export interface Runtime {
  id: string;
  name: string;
  base: string;
  custom: boolean;
  protocol: "claude-stream-json" | "codex-app-server" | "acp";
  verdict: "ok" | "missing" | "below_minimum" | "not_executable" | string;
  version?: string | null;
  path?: string;
  detail?: string | null;
  login?: { state: "logged_in" | "logged_out" | "unknown"; method?: string | null; plan?: string | null } | null;
  caps: { interrupt: boolean; resume: boolean; partial: boolean; thinking: boolean; questions: boolean };
}

export interface RuntimesReport {
  host: { can_start: boolean; reason: string | null; enabled?: boolean; max_live?: number; config_error?: string | null };
  runtimes: Runtime[];
}

/** A session on a host, as `chat_sessions` reports it. */
export interface SessionInfo {
  id: string;
  runtime: string;
  name: string;
  cwd: string;
  model?: string | null;
  state: ChatState;
  by: { device: string; principal?: string | null };
  started_at: number;
  turns: number;
  last_activity: number;
  ended_at?: number | null;
  end_reason?: string | null;
  seq: number;
}

export type ToolContent = { type: "text"; text: string } | { type: "diff"; path: string; old: string | null; new: string };

/** One event of a chat (see the host's chat/event.rs). */
export type ChatEvent = { seq: number; ts: number; turn: number } & (
  | { t: "user"; input_id: string; by: { device: string }; text: string }
  | { t: "turn_start" }
  | { t: "message"; id: string; text: string; delta: boolean }
  | { t: "thought"; id: string; text: string; delta: boolean }
  | { t: "tool_call"; call_id: string; title: string; kind: ToolKind; status: ToolStatus; input?: unknown; locations?: string[]; content?: ToolContent[] }
  | { t: "tool_update"; call_id: string; status?: ToolStatus; content?: ToolContent[]; truncated?: boolean }
  | { t: "plan"; entries: Array<{ content: string; status: string }> }
  | { t: "question"; qid: string; prompt: string; options?: string[] }
  | { t: "usage"; input_tokens?: number; output_tokens?: number; cost_usd?: number }
  | { t: "turn_end"; status: TurnStatus; final_text?: string; duration_ms: number; reason?: string }
  | { t: "state"; state: ChatState; queued: Array<{ id: string; text: string }> }
  | { t: "notice"; level: "info" | "warn" | "error" | "debug"; code: string; text: string }
);

/** What the host answered when the chat channel opened. */
export interface ChatHello {
  session: string;
  role: "rw" | "ro";
  runtime: { id: string; name: string };
  cwd: string;
  state: ChatState;
  seq: number;
  caps: Runtime["caps"] | null;
  by?: { device: string };
  host?: string;
}

export interface ToolItem {
  kind: "tool";
  callId: string;
  title: string;
  tool: ToolKind;
  status: ToolStatus;
  input?: unknown;
  locations: string[];
  content: ToolContent[];
  truncated: boolean;
  startedTs: number;
  endedTs: number | null;
}

/** One block of a turn, in the order it happened. */
export type Block =
  | { kind: "text"; id: string; text: string }
  | { kind: "thought"; id: string; text: string; startedTs: number; endedTs: number }
  | ToolItem
  | { kind: "plan"; entries: Array<{ content: string; status: string }> }
  | { kind: "question"; qid: string; prompt: string; options: string[]; answered: boolean }
  | { kind: "notice"; level: string; code: string; text: string };

export interface Turn {
  n: number;
  user: { text: string; by: string; ts: number } | null;
  blocks: Block[];
  status: TurnStatus | "running";
  finalText: string | null;
  durationMs: number | null;
  reason: string | null;
  usage: { input?: number; output?: number; cost?: number } | null;
}

export interface Chat {
  seq: number;
  state: ChatState;
  queued: Array<{ id: string; text: string }>;
  turns: Turn[];
  /** Notices outside any turn (a restart, a resumed conversation). */
  notices: Array<{ level: string; code: string; text: string; ts: number }>;
}

export const emptyChat = (): Chat => ({ seq: 0, state: "idle", queued: [], turns: [], notices: [] });

/** Turns kept in memory; older ones are dropped from the panel (the host keeps the transcript). */
export const MAX_TURNS = 200;

function currentTurn(c: Chat, n: number): Turn {
  let turn = c.turns[c.turns.length - 1];
  if (!turn || turn.n !== n) {
    turn = { n, user: null, blocks: [], status: "running", finalText: null, durationMs: null, reason: null, usage: null };
    c.turns.push(turn);
    if (c.turns.length > MAX_TURNS) c.turns.splice(0, c.turns.length - MAX_TURNS);
  }
  return turn;
}

/** Folds one event into the chat; returns a new chat object (blocks are copied on write). */
export function reduceChat(prev: Chat, e: ChatEvent): Chat {
  if (e.seq < 0) {
    // Not part of the conversation (a refused command): shown, never numbered.
    if (e.t !== "notice") return prev;
    return { ...prev, notices: [...prev.notices.slice(-20), { level: e.level, code: e.code, text: e.text, ts: e.ts }] };
  }
  if (e.seq <= prev.seq) return prev; // a replay overlapping what is already here
  const c: Chat = { ...prev, seq: e.seq, turns: prev.turns.slice(), notices: prev.notices };
  const touch = (n: number) => {
    const i = c.turns.findIndex((t) => t.n === n);
    if (i >= 0) {
      c.turns[i] = { ...c.turns[i], blocks: c.turns[i].blocks.slice() };
      return c.turns[i];
    }
    return currentTurn(c, n);
  };
  switch (e.t) {
    case "state":
      c.state = e.state;
      c.queued = e.queued ?? [];
      return c;
    case "user": {
      // The user line comes right before its turn starts (whose number is one more).
      const turn = touch(e.turn + 1);
      turn.user = { text: e.text, by: e.by?.device ?? "", ts: e.ts };
      return c;
    }
    case "turn_start":
      touch(e.turn);
      return c;
    case "notice":
      if (e.level === "debug") return c;
      if (e.turn > 0 && c.turns.some((t) => t.n === e.turn && t.status === "running")) {
        touch(e.turn).blocks.push({ kind: "notice", level: e.level, code: e.code, text: e.text });
      } else {
        c.notices = [...c.notices.slice(-20), { level: e.level, code: e.code, text: e.text, ts: e.ts }];
      }
      return c;
    default:
      break;
  }
  const turn = touch(e.turn);
  const b = turn.blocks;
  switch (e.t) {
    case "message":
    case "thought": {
      const kind = e.t === "message" ? "text" : "thought";
      const i = b.findIndex((x) => x.kind === kind && x.id === e.id);
      if (i >= 0) {
        const old = b[i] as Extract<Block, { kind: "text" | "thought" }>;
        b[i] = kind === "thought"
          ? { ...(old as Extract<Block, { kind: "thought" }>), text: e.delta ? old.text + e.text : e.text, endedTs: e.ts }
          : { ...old, text: e.delta ? old.text + e.text : e.text };
      } else if (kind === "thought") {
        b.push({ kind: "thought", id: e.id, text: e.text, startedTs: e.ts, endedTs: e.ts });
      } else {
        b.push({ kind: "text", id: e.id, text: e.text });
      }
      break;
    }
    case "tool_call":
      b.push({ kind: "tool", callId: e.call_id, title: e.title, tool: e.kind, status: e.status, input: e.input,
        locations: e.locations ?? [], content: e.content ?? [], truncated: false, startedTs: e.ts,
        endedTs: e.status === "completed" || e.status === "failed" ? e.ts : null });
      break;
    case "tool_update": {
      const i = b.findIndex((x) => x.kind === "tool" && x.callId === e.call_id);
      if (i < 0) break;
      const old = b[i] as ToolItem;
      const status = e.status ?? old.status;
      b[i] = { ...old, status, content: e.content?.length ? [...old.content, ...e.content] : old.content,
        truncated: old.truncated || !!e.truncated,
        endedTs: status === "completed" || status === "failed" ? e.ts : old.endedTs };
      break;
    }
    case "plan": {
      const i = b.findIndex((x) => x.kind === "plan");
      if (i >= 0) b[i] = { kind: "plan", entries: e.entries };
      else b.push({ kind: "plan", entries: e.entries });
      break;
    }
    case "question":
      b.push({ kind: "question", qid: e.qid, prompt: e.prompt, options: e.options ?? [], answered: false });
      break;
    case "usage":
      turn.usage = { input: e.input_tokens, output: e.output_tokens, cost: e.cost_usd };
      break;
    case "turn_end":
      turn.status = e.status;
      turn.finalText = e.final_text ?? null;
      turn.durationMs = e.duration_ms;
      turn.reason = e.reason ?? null;
      // Tools still shown as running did not report an end; the turn's end is theirs.
      for (let i = 0; i < b.length; i++) {
        const x = b[i];
        if (x.kind === "tool" && (x.status === "in_progress" || x.status === "pending")) {
          b[i] = { ...x, status: e.status === "completed" ? "completed" : "failed", endedTs: e.ts };
        }
      }
      break;
    default:
      break;
  }
  return c;
}

/** Marks a question answered (the answer goes to the host separately). */
export function markAnswered(prev: Chat, qid: string): Chat {
  return { ...prev, turns: prev.turns.map((t) => ({ ...t, blocks: t.blocks.map((b) => (b.kind === "question" && b.qid === qid ? { ...b, answered: true } : b)) })) };
}

/**
 * A turn split as the panel shows it: what the agent said before its first tool (the preamble), the
 * steps (tools, thoughts, plans, the text between them) folded into one row, and its last words (the
 * answer). A turn without tools is all answer.
 */
export function splitTurn(turn: Turn): { preamble: Block[]; steps: Block[]; answer: Block[] } {
  const b = turn.blocks;
  const firstTool = b.findIndex((x) => x.kind === "tool" || x.kind === "plan");
  if (firstTool < 0) return { preamble: [], steps: b.filter((x) => x.kind === "thought"), answer: b.filter((x) => x.kind !== "thought") };
  let lastTool = -1;
  b.forEach((x, i) => {
    if (x.kind === "tool" || x.kind === "plan") lastTool = i;
  });
  const after = b.slice(lastTool + 1);
  const answerStart = turn.status === "running" ? after.length : after.findIndex((x) => x.kind === "text");
  const answer = answerStart >= 0 ? after.slice(answerStart).filter((x) => x.kind !== "thought") : [];
  const answerSet = new Set<Block>(answer);
  return {
    preamble: b.slice(0, firstTool).filter((x) => x.kind === "text"),
    steps: b.filter((x, i) => (i >= firstTool || x.kind === "thought") && !answerSet.has(x) && !(i < firstTool && x.kind === "text")),
    answer,
  };
}

/** What the agent is doing now, for the status line: the last running tool, thinking, or writing. */
export function phase(chat: Chat): { what: "tool" | "thinking" | "writing" | "starting" | "stopping" | null; title?: string; since?: number } {
  if (chat.state === "starting") return { what: "starting" };
  if (chat.state === "interrupting") return { what: "stopping" };
  if (chat.state !== "running") return { what: null };
  const turn = chat.turns[chat.turns.length - 1];
  const last = turn?.blocks[turn.blocks.length - 1];
  if (!last) return { what: "thinking" };
  if (last.kind === "tool" && last.status === "in_progress") return { what: "tool", title: last.title, since: last.startedTs };
  if (last.kind === "thought") return { what: "thinking", since: last.startedTs };
  if (last.kind === "text") return { what: "writing" };
  return { what: "thinking" };
}

/** The pieces of a message for safe rendering: plain text and fenced code. No HTML is ever produced. */
export function textParts(text: string): Array<{ code: boolean; lang?: string; text: string }> {
  const parts: Array<{ code: boolean; lang?: string; text: string }> = [];
  const re = /```([\w+-]*)\n([\s\S]*?)(?:```|$)/g;
  let at = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    if (m.index > at) parts.push({ code: false, text: text.slice(at, m.index) });
    parts.push({ code: true, lang: m[1] || undefined, text: m[2].replace(/\n$/, "") });
    at = re.lastIndex;
  }
  if (at < text.length) parts.push({ code: false, text: text.slice(at) });
  return parts.filter((p) => p.code || p.text.trim().length > 0);
}

/** Inline `code` spans of a plain-text piece. */
export function inlineParts(text: string): Array<{ code: boolean; text: string }> {
  return text.split(/(`[^`\n]+`)/g).filter(Boolean).map((s) => (s.length > 2 && s.startsWith("`") && s.endsWith("`") ? { code: true, text: s.slice(1, -1) } : { code: false, text: s }));
}

/** Lines of a simple diff between two texts: removed lines then added lines, common head and tail kept. */
export function diffLines(oldText: string | null, newText: string): Array<{ sign: " " | "-" | "+"; text: string }> {
  const a = oldText === null ? [] : oldText.split("\n");
  const b = newText.split("\n");
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head++;
  let tail = 0;
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail++;
  const out: Array<{ sign: " " | "-" | "+"; text: string }> = [];
  for (const l of a.slice(Math.max(0, head - 2), head)) out.push({ sign: " ", text: l });
  for (const l of a.slice(head, a.length - tail)) out.push({ sign: "-", text: l });
  for (const l of b.slice(head, b.length - tail)) out.push({ sign: "+", text: l });
  for (const l of b.slice(b.length - tail, Math.min(b.length, b.length - tail + 2))) out.push({ sign: " ", text: l });
  return out;
}

/** Counts of added and removed lines of a tool's diffs, for its summary. */
export function diffStat(content: ToolContent[]): { add: number; del: number } | null {
  const diffs = content.filter((c): c is Extract<ToolContent, { type: "diff" }> => c.type === "diff");
  if (!diffs.length) return null;
  let add = 0;
  let del = 0;
  for (const d of diffs) {
    for (const l of diffLines(d.old, d.new)) {
      if (l.sign === "+") add++;
      else if (l.sign === "-") del++;
    }
  }
  return { add, del };
}

/** The last folder used for an agent on a host, remembered between runs. */
const CWD_KEY = "ai-remote.chat.cwd";

export function lastCwd(host: string): string {
  try {
    const all = JSON.parse(localStorage.getItem(CWD_KEY) ?? "{}") as Record<string, string[]>;
    return all[host]?.[0] ?? "";
  } catch {
    return "";
  }
}

export function recentCwds(host: string): string[] {
  try {
    const all = JSON.parse(localStorage.getItem(CWD_KEY) ?? "{}") as Record<string, string[]>;
    return all[host] ?? [];
  } catch {
    return [];
  }
}

export function rememberCwd(host: string, cwd: string) {
  try {
    const all = JSON.parse(localStorage.getItem(CWD_KEY) ?? "{}") as Record<string, string[]>;
    all[host] = [cwd, ...(all[host] ?? []).filter((x) => x !== cwd)].slice(0, 6);
    localStorage.setItem(CWD_KEY, JSON.stringify(all));
  } catch {
    // Storage full or unavailable: nothing to remember.
  }
}
