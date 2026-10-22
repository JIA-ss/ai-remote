// Coding-agent chats on a host (Claude Code, Codex, ACP agents): the host's events and how they fold
// into what the chat panel shows. Pure, so the folding is testable without a page
// (tools/chat-state.test.mjs). The transport is in api.ts (`openChat`).
//
// The panel's layout follows Orca's native chat (stablyai/orca, MIT): a turn is a list of rows —
// the user's message, the agent's prose, a folded "run" of tool calls summarized in one sentence
// (edits included: they open to diff cards), reasoning folded behind "Thought for 4 s". `turnRows`
// is that projection.

export type ChatState = "starting" | "idle" | "running" | "interrupting" | "parked" | "ended";
export type ToolKind = "read" | "edit" | "delete" | "move" | "search" | "execute" | "think" | "fetch" | "other";
export type ToolStatus = "pending" | "in_progress" | "completed" | "failed";
export type TurnStatus = "completed" | "failed" | "interrupted";
/** How a message sent while a turn runs is handled: after it, stopping it, or added to it. */
export type InputMode = "queue" | "interrupt" | "steer";

export interface Caps {
  interrupt: boolean;
  resume: boolean;
  partial: boolean;
  thinking: boolean;
  questions: boolean;
  /** A message can be added to the running turn (`mode: "steer"`); hosts without it queue. */
  steer?: boolean;
}

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
  caps: Caps;
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
export interface PlanEntry {
  content: string;
  status: string;
}

/** One event of a chat (see the host's chat/event.rs). */
export type ChatEvent = { seq: number; ts: number; turn: number } & (
  | { t: "user"; input_id: string; by: { device: string }; text: string; steer?: boolean }
  | { t: "turn_start" }
  | { t: "message"; id: string; text: string; delta: boolean }
  | { t: "thought"; id: string; text: string; delta: boolean }
  | { t: "tool_call"; call_id: string; title: string; kind: ToolKind; status: ToolStatus; input?: unknown; locations?: string[]; content?: ToolContent[] }
  | { t: "tool_update"; call_id: string; status?: ToolStatus; content?: ToolContent[]; truncated?: boolean }
  | { t: "plan"; entries: PlanEntry[] }
  | { t: "question"; qid: string; prompt: string; options?: string[] }
  | { t: "usage"; input_tokens?: number; output_tokens?: number; cost_usd?: number; context_window?: number; context_used?: number }
  | { t: "turn_end"; status: TurnStatus; final_text?: string; duration_ms: number; reason?: string }
  | { t: "state"; state: ChatState; queued: Array<{ id: string; text: string }> }
  | { t: "notice"; level: "info" | "warn" | "error" | "debug"; code: string; text: string; input_id?: string }
);

/**
 * A line from the host that is not part of the conversation: `ack` (a message reached the host),
 * `error` (a refused command; `input_id` names the message it refused), and answers to requests
 * carrying a `rid` (see `files`).
 */
export type ChatReply = { t: string; input_id?: string; rid?: string; code?: string; text?: string; [k: string]: unknown };

/**
 * Requests that the host answers with a reply carrying the same `rid` (`{"files":{...}}`): `ask`
 * sends `{[name]: {...args, rid}}` and resolves with the reply; `reply` feeds replies in and says
 * whether one was a request's answer.
 */
export function requester(send: (command: Record<string, unknown>) => Promise<void>, timeoutMs = 10_000) {
  let next = 0;
  const waiting = new Map<string, { ok: (r: ChatReply) => void; fail: (e: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  return {
    ask(name: string, args: Record<string, unknown>): Promise<ChatReply> {
      const rid = `r${++next}`;
      return new Promise<ChatReply>((ok, fail) => {
        const timer = setTimeout(() => {
          waiting.delete(rid);
          fail(new Error("no answer from the host"));
        }, timeoutMs);
        waiting.set(rid, { ok, fail, timer });
        send({ [name]: { ...args, rid } }).catch((e) => {
          clearTimeout(timer);
          waiting.delete(rid);
          fail(e instanceof Error ? e : new Error(String(e)));
        });
      });
    },
    reply(m: ChatReply): boolean {
      const w = typeof m.rid === "string" ? waiting.get(m.rid) : undefined;
      if (!w) return false;
      clearTimeout(w.timer);
      waiting.delete(m.rid as string);
      if (m.t === "error") w.fail(new Error(m.text ?? "refused"));
      else w.ok(m);
      return true;
    },
    close() {
      for (const w of waiting.values()) {
        clearTimeout(w.timer);
        w.fail(new Error("the chat closed"));
      }
      waiting.clear();
    },
  };
}

/** What a line from the chat channel is: the end of the chat, a numbered event, or a reply. */
export function chatLineKind(m: unknown): "closed" | "event" | "reply" | "notice" | null {
  if (!m || typeof m !== "object") return null;
  const e = m as { t?: unknown; seq?: unknown; input_id?: unknown; rid?: unknown };
  if (e.t === "closed" || e.t === "bye") return "closed";
  if (typeof e.seq === "number") return "event";
  if (e.t === "ack" || e.rid !== undefined || (e.t === "error" && typeof e.input_id === "string")) return "reply";
  if (e.t === "error") return "notice";
  return null;
}

/** What the host answered when the chat channel opened. */
export interface ChatHello {
  session: string;
  role: "rw" | "ro";
  runtime: { id: string; name: string };
  cwd: string;
  state: ChatState;
  seq: number;
  caps: Caps | null;
  by?: { device: string };
  host?: string;
  /** The host answers every input with an `ack` (or a refusal naming it). */
  acks?: boolean;
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
  /** Cut short because the turn was stopped: not a failure of the tool. */
  stopped?: boolean;
}

export type ThoughtBlock = { kind: "thought"; id: string; text: string; startedTs: number; endedTs: number };
export type QuestionBlock = { kind: "question"; qid: string; prompt: string; options: string[]; answered: boolean; answer?: string };

/** One block of a turn, in the order it happened. */
export type Block =
  | { kind: "text"; id: string; text: string }
  | ThoughtBlock
  | ToolItem
  | { kind: "plan"; entries: PlanEntry[] }
  | QuestionBlock
  | { kind: "notice"; level: string; code: string; text: string }
  /** A message added to the running turn (`steer`). */
  | { kind: "user"; text: string; by: string; ts: number; steer: true };

export interface Usage {
  input?: number;
  output?: number;
  cost?: number;
  contextWindow?: number;
  contextUsed?: number;
}

export interface Turn {
  n: number;
  user: { text: string; by: string; ts: number } | null;
  blocks: Block[];
  status: TurnStatus | "running";
  startedTs: number | null;
  finalText: string | null;
  durationMs: number | null;
  reason: string | null;
  usage: Usage | null;
}

export interface Chat {
  seq: number;
  state: ChatState;
  queued: Array<{ id: string; text: string }>;
  turns: Turn[];
  /** Notices outside any turn (a restart, a resumed conversation). */
  notices: Array<{ level: string; code: string; text: string; ts: number }>;
  /** How full the agent's context window is, from its last report that had both numbers. */
  context: { used: number; window: number } | null;
  /** Ids of the latest messages the host accepted into the conversation (`user` events). */
  inputs: string[];
  /** Messages the host dropped after accepting them (the queue was full), with why. */
  dropped: Array<{ id: string; text: string }>;
}

export const emptyChat = (): Chat => ({ seq: 0, state: "idle", queued: [], turns: [], notices: [], context: null, inputs: [], dropped: [] });

/** Turns kept in memory; older ones are dropped from the panel (the host keeps the transcript). */
export const MAX_TURNS = 200;

function newTurn(n: number): Turn {
  return { n, user: null, blocks: [], status: "running", startedTs: null, finalText: null, durationMs: null, reason: null, usage: null };
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
  // Only the turn an event touches is copied, so the panel re-renders that turn alone.
  const touch = (n: number) => {
    const i = c.turns.findIndex((t) => t.n === n);
    if (i >= 0) {
      c.turns[i] = { ...c.turns[i], blocks: c.turns[i].blocks.slice() };
      return c.turns[i];
    }
    const turn = newTurn(n);
    let at = c.turns.length;
    while (at > 0 && c.turns[at - 1].n > n) at--;
    c.turns.splice(at, 0, turn);
    if (c.turns.length > MAX_TURNS) c.turns.splice(0, c.turns.length - MAX_TURNS);
    return turn;
  };
  switch (e.t) {
    case "state":
      c.state = e.state;
      c.queued = e.queued ?? [];
      return c;
    case "user": {
      if (e.input_id) c.inputs = [...c.inputs.slice(-99), e.input_id];
      if (e.steer) {
        // Added to a running turn: shown where it landed, inside that turn. The host numbers it with
        // the running turn; an older host may number it as the next one, so the running turn wins.
        const running = [...c.turns].reverse().find((t) => t.status === "running");
        const target = running ? running.n : c.turns.some((t) => t.n === e.turn) ? e.turn : e.turn + 1;
        touch(target).blocks.push({ kind: "user", text: e.text, by: e.by?.device ?? "", ts: e.ts, steer: true });
        return c;
      }
      // The user line comes right before its turn starts (whose number is one more).
      const turn = touch(e.turn + 1);
      turn.user = { text: e.text, by: e.by?.device ?? "", ts: e.ts };
      return c;
    }
    case "turn_start":
      touch(e.turn).startedTs = e.ts;
      return c;
    case "notice":
      if (e.input_id) c.dropped = [...c.dropped.slice(-19), { id: e.input_id, text: e.text }];
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
  if (turn.startedTs === null) turn.startedTs = e.ts;
  const b = turn.blocks;
  switch (e.t) {
    case "message":
    case "thought": {
      const kind = e.t === "message" ? "text" : "thought";
      const i = b.findIndex((x) => x.kind === kind && x.id === e.id);
      if (i >= 0) {
        const old = b[i] as Extract<Block, { kind: "text" | "thought" }>;
        b[i] = kind === "thought"
          ? { ...(old as ThoughtBlock), text: e.delta ? old.text + e.text : e.text, endedTs: e.ts }
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
      // One plan per turn, updated in place: the agent rewrites its whole list each time.
      const i = b.findIndex((x) => x.kind === "plan");
      if (i >= 0) b[i] = { kind: "plan", entries: e.entries };
      else b.push({ kind: "plan", entries: e.entries });
      break;
    }
    case "question":
      b.push({ kind: "question", qid: e.qid, prompt: e.prompt, options: e.options ?? [], answered: false });
      break;
    case "usage": {
      turn.usage = { input: e.input_tokens, output: e.output_tokens, cost: e.cost_usd, contextWindow: e.context_window, contextUsed: e.context_used };
      if (typeof e.context_window === "number" && typeof e.context_used === "number" && e.context_window > 0) {
        c.context = { used: e.context_used, window: e.context_window };
      }
      break;
    }
    case "turn_end":
      turn.status = e.status;
      turn.finalText = e.final_text ?? null;
      turn.durationMs = e.duration_ms;
      turn.reason = e.reason ?? null;
      // Tools still shown as running did not report an end; the turn's end is theirs.
      for (let i = 0; i < b.length; i++) {
        const x = b[i];
        if (x.kind === "tool" && (x.status === "in_progress" || x.status === "pending")) {
          b[i] = { ...x, status: e.status === "completed" ? "completed" : "failed", endedTs: e.ts, ...(e.status === "interrupted" ? { stopped: true } : {}) };
        }
      }
      break;
    default:
      break;
  }
  return c;
}

/** Marks a question answered (the answer goes to the host separately). */
export function markAnswered(prev: Chat, qid: string, answer?: string): Chat {
  return {
    ...prev,
    turns: prev.turns.map((t) =>
      t.blocks.some((b) => b.kind === "question" && b.qid === qid)
        ? { ...t, blocks: t.blocks.map((b) => (b.kind === "question" && b.qid === qid ? { ...b, answered: true, answer } : b)) }
        : t,
    ),
  };
}

// ---------------------------------------------------------------------------------------------
// Rows: what a turn shows, in order.

/** A sub-agent the agent started (Claude's Task tool); shown as its own row, not folded in a run. */
export function isSubagent(x: ToolItem): boolean {
  return x.tool === "think" && /^(Task|Agent)\b/.test(x.title);
}

/** An edit with a diff to show: a diff card inside its run. */
export function isEdit(x: ToolItem): boolean {
  return x.tool === "edit" || x.content.some((c) => c.type === "diff");
}

export type Row =
  | { kind: "user"; key: string; text: string; by: string; ts: number; steer: boolean }
  | { kind: "text"; key: string; text: string; streaming: boolean }
  | { kind: "thought"; key: string; block: ThoughtBlock; live: boolean }
  /** Tool calls in a row (with the reasoning between them), summarized in one sentence. */
  | { kind: "run"; key: string; members: Array<ToolItem | ThoughtBlock>; tools: ToolItem[]; live: boolean }
  | { kind: "agent"; key: string; tool: ToolItem }
  | { kind: "plan"; key: string; entries: PlanEntry[] }
  | { kind: "question"; key: string; block: QuestionBlock }
  | { kind: "notice"; key: string; level: string; code: string; text: string };

const joinsRun = (x: Block) => x.kind === "tool" && !isSubagent(x);

/** The rows of a turn. A running turn's last run and last text are live. */
export function turnRows(turn: Turn): Row[] {
  const rows: Row[] = [];
  const running = turn.status === "running";
  if (turn.user) rows.push({ kind: "user", key: `u${turn.n}`, text: turn.user.text, by: turn.user.by, ts: turn.user.ts, steer: false });
  const b = turn.blocks;
  let run: Extract<Row, { kind: "run" }> | null = null;
  for (let i = 0; i < b.length; i++) {
    const x = b[i];
    if (joinsRun(x)) {
      const tool = x as ToolItem;
      if (!run) {
        run = { kind: "run", key: `r${turn.n}:${tool.callId}`, members: [], tools: [], live: false };
        rows.push(run);
      }
      run.members.push(tool);
      run.tools.push(tool);
      continue;
    }
    if (x.kind === "thought") {
      // Reasoning between two tool calls stays inside their run; anywhere else it is its own row.
      let next = i + 1;
      while (next < b.length && b[next].kind === "thought") next++;
      if (run && next < b.length && joinsRun(b[next])) {
        run.members.push(x);
        continue;
      }
      run = null;
      rows.push({ kind: "thought", key: `t${turn.n}:${x.id}`, block: x, live: false });
      continue;
    }
    run = null;
    switch (x.kind) {
      case "text":
        if (x.text.trim()) rows.push({ kind: "text", key: `m${turn.n}:${x.id}`, text: x.text, streaming: false });
        break;
      case "tool":
        rows.push({ kind: "agent", key: `a${turn.n}:${x.callId}`, tool: x });
        break;
      case "plan":
        rows.push({ kind: "plan", key: `p${turn.n}`, entries: x.entries });
        break;
      case "question":
        rows.push({ kind: "question", key: `q${turn.n}:${x.qid}`, block: x });
        break;
      case "notice":
        rows.push({ kind: "notice", key: `n${turn.n}:${i}`, level: x.level, code: x.code, text: x.text });
        break;
      case "user":
        rows.push({ kind: "user", key: `s${turn.n}:${i}`, text: x.text, by: x.by, ts: x.ts, steer: true });
        break;
    }
  }
  if (running) {
    const last = rows[rows.length - 1];
    if (last?.kind === "run") last.live = true;
    if (last?.kind === "text") last.streaming = true;
    if (last?.kind === "thought") last.live = true;
  }
  for (const r of rows) if (r.kind === "run" && r.tools.some((x) => x.status === "in_progress" || x.status === "pending")) r.live = running;
  return rows;
}

/** What a run did, by kind, in the order each kind first appears: "ran 2 commands, read 1 file". */
export type RunCategory = "command" | "read" | "search" | "web" | "change" | "tool";

export function runSummary(tools: ToolItem[]): Array<{ cat: RunCategory; n: number }> {
  const out: Array<{ cat: RunCategory; n: number }> = [];
  for (const x of tools) {
    const cat: RunCategory =
      x.tool === "execute" ? "command"
        : x.tool === "read" ? "read"
          : x.tool === "search" ? "search"
            : x.tool === "fetch" ? "web"
              : isEdit(x) || x.tool === "delete" || x.tool === "move" ? "change"
                : "tool";
    const have = out.find((o) => o.cat === cat);
    if (have) have.n++;
    else out.push({ cat, n: 1 });
  }
  return out;
}

/** Lines added and removed by the edits of a run, for its header (null when it edited nothing). */
export function runDiffStat(tools: ToolItem[]): { add: number; del: number } | null {
  let add = 0;
  let del = 0;
  let any = false;
  for (const x of tools) {
    const st = diffStat(x.content);
    if (!st) continue;
    any = true;
    add += st.add;
    del += st.del;
  }
  return any ? { add, del } : null;
}

/** How a run ended: calls that failed, calls cut short by Stop, and whether all of them finished. */
export function runOutcome(tools: ToolItem[]): { failed: number; stopped: number; done: boolean } {
  return {
    failed: tools.filter((x) => x.status === "failed" && !x.stopped).length,
    stopped: tools.filter((x) => x.stopped).length,
    done: tools.every((x) => x.status === "completed" || x.status === "failed"),
  };
}

/** A tool's title split into the verb the agent used and what it acted on ("Bash", "npm test"). */
export function toolLabel(x: ToolItem): { verb: string; target: string } {
  const m = /^(\S+)\s+([\s\S]+)$/.exec(x.title.trim());
  if (!m) return { verb: x.title.trim(), target: "" };
  return { verb: m[1], target: m[2] };
}

/** The command of a shell tool, from its input or its title. */
export function toolCommand(x: ToolItem): string | null {
  if (x.tool !== "execute") return null;
  const input = x.input as { command?: unknown; cmd?: unknown } | undefined;
  const cmd = input?.command ?? input?.cmd;
  if (typeof cmd === "string") return cmd;
  if (Array.isArray(cmd)) return cmd.map(String).join(" ");
  return toolLabel(x).target || null;
}

/** The output of a tool as one text (its text contents joined). */
export function toolOutput(x: ToolItem): string {
  return x.content.filter((c): c is Extract<ToolContent, { type: "text" }> => c.type === "text").map((c) => c.text).join("\n");
}

/** The last `n` lines of a text and how many lines came before them. */
export function tailLines(text: string, n: number): { text: string; hidden: number } {
  const lines = text.replace(/\n+$/, "").split("\n");
  if (lines.length <= n) return { text: lines.join("\n"), hidden: 0 };
  return { text: lines.slice(-n).join("\n"), hidden: lines.length - n };
}

/** What the agent is doing now, for the status line: the last running tool, thinking, or writing. */
export function phase(chat: Chat): { what: "tool" | "thinking" | "writing" | "asking" | "starting" | "stopping" | null; title?: string; since?: number } {
  if (chat.state === "starting") return { what: "starting" };
  if (chat.state === "interrupting") return { what: "stopping" };
  if (chat.state !== "running") return { what: null };
  const turn = chat.turns[chat.turns.length - 1];
  const last = turn?.blocks[turn.blocks.length - 1];
  const since = turn?.startedTs ?? undefined;
  if (!last) return { what: "thinking", since };
  if (turn.blocks.some((b) => b.kind === "question" && !b.answered)) return { what: "asking", since };
  if (last.kind === "tool" && last.status === "in_progress") return { what: "tool", title: last.title, since };
  if (last.kind === "text") return { what: "writing", since };
  return { what: "thinking", since };
}

/** The turn that was stopped last, if nothing ran since: the panel offers to continue it. */
export function stoppedTurn(chat: Chat): Turn | null {
  const last = chat.turns[chat.turns.length - 1];
  if (!last || last.status !== "interrupted" || chat.state === "running" || chat.state === "ended" || chat.queued.length) return null;
  return last;
}

// ---------------------------------------------------------------------------------------------
// Diffs

export type DiffRow =
  | { kind: "context" | "add" | "del"; text: string }
  /** Lines left out between two changes. */
  | { kind: "gap"; lines: number };

/** Lines kept around each change. */
const CONTEXT = 3;
/** Above this many cells the line diff falls back to "all removed, all added" for the middle. */
const MAX_CELLS = 4_000_000;

/** The edit script between two lists of lines (longest common subsequence). */
function editScript(a: string[], b: string[]): Array<{ kind: "context" | "add" | "del"; text: string }> {
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head++;
  let tail = 0;
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail++;
  const out: Array<{ kind: "context" | "add" | "del"; text: string }> = a.slice(0, head).map((text) => ({ kind: "context" as const, text }));
  const am = a.slice(head, a.length - tail);
  const bm = b.slice(head, b.length - tail);
  const n = am.length;
  const m = bm.length;
  if (n && m && (n + 1) * (m + 1) <= MAX_CELLS) {
    // lcs[i][j]: length of the common subsequence of am[i..] and bm[j..].
    const w = m + 1;
    const lcs = new Uint32Array((n + 1) * w);
    for (let i = n - 1; i >= 0; i--) {
      for (let j = m - 1; j >= 0; j--) {
        lcs[i * w + j] = am[i] === bm[j] ? lcs[(i + 1) * w + j + 1] + 1 : Math.max(lcs[(i + 1) * w + j], lcs[i * w + j + 1]);
      }
    }
    let i = 0;
    let j = 0;
    while (i < n && j < m) {
      if (am[i] === bm[j]) {
        out.push({ kind: "context", text: am[i] });
        i++;
        j++;
      } else if (lcs[(i + 1) * w + j] >= lcs[i * w + j + 1]) {
        out.push({ kind: "del", text: am[i++] });
      } else {
        out.push({ kind: "add", text: bm[j++] });
      }
    }
    while (i < n) out.push({ kind: "del", text: am[i++] });
    while (j < m) out.push({ kind: "add", text: bm[j++] });
  } else {
    for (const text of am) out.push({ kind: "del", text });
    for (const text of bm) out.push({ kind: "add", text });
  }
  for (const text of a.slice(a.length - tail)) out.push({ kind: "context", text });
  return out;
}

/** The rows of a diff card: changes with a few lines around them; stretches between them fold into gaps. */
export function diffRows(oldText: string | null, newText: string): DiffRow[] {
  const a = oldText === null || oldText === "" ? [] : oldText.split("\n");
  const b = newText === "" ? [] : newText.split("\n");
  const script = editScript(a, b);
  const keep = new Array<boolean>(script.length).fill(false);
  script.forEach((l, i) => {
    if (l.kind === "context") return;
    for (let k = Math.max(0, i - CONTEXT); k <= Math.min(script.length - 1, i + CONTEXT); k++) keep[k] = true;
  });
  const out: DiffRow[] = [];
  let skipped = 0;
  // Only the stretch between two changes is marked; lines left out before the first or after the
  // last change are simply not shown (as in Orca's diff card).
  script.forEach((l, i) => {
    if (keep[i]) {
      if (skipped && out.length) out.push({ kind: "gap", lines: skipped });
      skipped = 0;
      out.push(l);
    } else {
      skipped++;
    }
  });
  return out;
}

/** Counts of added and removed lines of a tool's diffs, for its summary. */
export function diffStat(content: ToolContent[]): { add: number; del: number } | null {
  const diffs = content.filter((c): c is Extract<ToolContent, { type: "diff" }> => c.type === "diff");
  if (!diffs.length) return null;
  let add = 0;
  let del = 0;
  for (const d of diffs) {
    for (const l of diffRows(d.old, d.new)) {
      if (l.kind === "add") add++;
      else if (l.kind === "del") del++;
    }
  }
  return { add, del };
}

/** A diff as unified patch text, for the copy button. */
export function patchText(path: string, rows: DiffRow[]): string {
  const body = rows.map((r) => (r.kind === "gap" ? "@@" : `${r.kind === "add" ? "+" : r.kind === "del" ? "-" : " "}${r.text}`));
  return [`--- ${path}`, `+++ ${path}`, ...body].join("\n");
}

/** The file name of a path, Windows or Unix. */
export function baseName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
}

/** The host no longer has the session (it was restarted, or the session ended and was cleaned up). */
export function sessionGone(error: string): boolean {
  return /no chat session/i.test(error);
}

// ---------------------------------------------------------------------------------------------
// The composer

/** What Enter does while a turn runs: add to it when the agent can take that, else wait for it. */
export function sendMode(running: boolean, caps: Caps | null | undefined, interrupt = false): InputMode | undefined {
  if (!running) return undefined;
  if (interrupt) return "interrupt";
  return caps?.steer ? "steer" : "queue";
}

/** The input command for the host (see chat/1 in docs/architecture.md). */
export function inputCommand(id: string, text: string, mode?: InputMode): { input: { id: string; text: string; mode?: InputMode } } {
  return { input: { id, text, ...(mode && mode !== "queue" ? { mode } : {}) } };
}

/**
 * Editing a queued message: the host has no "replace", so the old one is taken back and the new
 * text queued again (it goes to the end of the queue).
 */
export function editQueued(queuedId: string, newId: string, text: string): Array<Record<string, unknown>> {
  return [{ dequeue: { id: queuedId } }, inputCommand(newId, text, "queue")];
}

// ---------------------------------------------------------------------------------------------
// Messages on their way: shown at the end of the conversation until the host has them.

/**
 * A message this device sent and the conversation does not show yet. `sending` until the host
 * acknowledges it (`acked`), then until it appears as a `user` event or in the queue; `failed` when
 * the host refused it, the connection dropped before the ack, or no ack came in time.
 */
export interface Outgoing {
  id: string;
  text: string;
  mode?: InputMode;
  at: number;
  state: "sending" | "acked" | "failed";
  error?: string;
  /** The host refused this id: a retry needs a new one (the host may remember this one). */
  refused?: boolean;
  /** Images sent with it, as data URLs for the bubble (the host keeps only their names). */
  images?: Array<{ name: string; url: string }>;
}

/** An unacknowledged message counts as not sent after this long (hosts that acknowledge only). */
export const SEND_TIMEOUT_MS = 15_000;

/**
 * Updates messages on their way against the conversation: delivered ones (a `user` event or a
 * queued entry with their id) leave, dropped ones fail, and unacknowledged ones fail after
 * [`SEND_TIMEOUT_MS`] when the host acknowledges messages at all.
 */
export function settleOutgoing(out: Outgoing[], chat: Chat, now: number, hostAcks: boolean): Outgoing[] {
  let changed = false;
  const next: Outgoing[] = [];
  for (const o of out) {
    if (chat.inputs.includes(o.id) || chat.queued.some((q) => q.id === o.id) || (o.state === "acked" && chat.state === "ended")) {
      changed = true;
      continue;
    }
    const dropped = chat.dropped.find((d) => d.id === o.id);
    if (dropped && o.state !== "failed") {
      next.push({ ...o, state: "failed", error: dropped.text, refused: true });
      changed = true;
    } else if (hostAcks && o.state === "sending" && now - o.at > SEND_TIMEOUT_MS) {
      next.push({ ...o, state: "failed", error: "timeout" });
      changed = true;
    } else next.push(o);
  }
  return changed ? next : out;
}

/** Applies a reply from the host to messages on their way (an `ack`, or a refusal naming one). */
export function outgoingReply(out: Outgoing[], m: ChatReply): Outgoing[] {
  if (!m.input_id) return out;
  const i = out.findIndex((o) => o.id === m.input_id);
  if (i < 0) return out;
  const next = out.slice();
  if (m.t === "ack") {
    if (next[i].state === "sending") next[i] = { ...next[i], state: "acked" };
  } else if (m.t === "error") {
    next[i] = { ...next[i], state: "failed", error: m.text ?? m.code ?? "refused", refused: true };
  }
  return next;
}

/** The connection dropped: what the host had not acknowledged is not sent. */
export function outgoingDisconnected(out: Outgoing[]): Outgoing[] {
  return out.some((o) => o.state === "sending") ? out.map((o) => (o.state === "sending" ? { ...o, state: "failed" as const, error: "disconnected" } : o)) : out;
}

/** Sending a failed message again: the same id when the host may have it (it ignores repeats), a new one after a refusal. */
export function retryOutgoing(o: Outgoing, newId: string, now: number): Outgoing {
  return { ...o, id: o.refused ? newId : o.id, state: "sending", error: undefined, refused: undefined, at: now };
}

/** Sent messages, newest last, for ↑ / ↓ in the composer. */
export function rememberSent(history: string[], text: string, max = 50): string[] {
  const v = text.trim();
  if (!v) return history;
  return [...history.filter((h) => h !== v), v].slice(-max);
}

/**
 * ↑ / ↓ through sent messages. `at` is the index being shown (history.length = the draft). Returns
 * the new index and text, or null when there is nowhere to go.
 */
export function browseHistory(history: string[], at: number, dir: -1 | 1, draft: string): { at: number; text: string } | null {
  const next = at + dir;
  if (next < 0 || next > history.length) return null;
  return { at: next, text: next === history.length ? draft : history[next] };
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

// ---- Folders on the host: the start form's folder picker and how a folder is shown ----
// Paths are the host's, so they may be Windows (`D:\work\x`, `\\server\share`) or Unix (`/home/x`),
// and `~` is the host user's home (the host expands it). Nothing here touches the local file system.

/** Whether `s` names a folder rather than being a search: absolute, `~`, a drive or a share. */
export function looksLikePath(s: string): boolean {
  const v = s.trim();
  return v === "~" || /^~[\\/]/.test(v) || v.startsWith("/") || /^[A-Za-z]:([\\/]|$)/.test(v) || v.startsWith("\\\\");
}

/** `p` without trailing separators, except where that is a root (`/`, `C:\`). */
export function trimFolder(p: string): string {
  const v = p.trim();
  const t = v.replace(/[\\/]+$/, "");
  if (!t) return v ? (v[0] === "\\" ? "\\" : "/") : "";
  if (/^[A-Za-z]:$/.test(t)) return t + (v.includes("/") && !v.includes("\\") ? "/" : "\\");
  return t;
}

/** The separator a path uses: a backslash once it has one (or is a bare drive), else a slash. */
function separator(p: string): string {
  return p.includes("\\") || /^[A-Za-z]:$/.test(p) ? "\\" : "/";
}

/** A path as its root (`/`, `C:\`, `\\server\share\`, `~`, or "" when relative) and the names below it. */
export function folderParts(p: string): { root: string; names: string[]; sep: string } {
  const v = trimFolder(p);
  const sep = separator(v);
  let root = "";
  let rest = v;
  const unc = /^\\\\[^\\/]+[\\/][^\\/]+/.exec(v);
  if (unc) root = unc[0] + "\\";
  else if (/^[A-Za-z]:/.test(v)) root = v.slice(0, 2) + (v[2] === "/" ? "/" : "\\");
  else if (v.startsWith("~")) root = "~";
  else if (/^[\\/]/.test(v)) root = v[0];
  rest = v.slice(Math.min(root.length, v.length));
  return { root, names: rest.split(/[\\/]+/).filter(Boolean), sep };
}

function buildFolder(root: string, names: string[], sep: string): string {
  if (!names.length) return root;
  if (!root) return names.join(sep);
  return (/[\\/]$/.test(root) ? root : root + sep) + names.join(sep);
}

/** What a folder is called: its last name (`app` for `D:\work\app`), or the root itself. */
export function folderName(p: string): string {
  const { root, names } = folderParts(p);
  if (names.length) return names[names.length - 1];
  const unc = /^\\\\[^\\]+\\([^\\]+)\\$/.exec(root);
  if (unc) return unc[1];
  return /^[A-Za-z]:[\\/]$/.test(root) ? root.slice(0, 2) : root;
}

/** The folder above `p`, or null at a root. */
export function parentFolder(p: string): string | null {
  const { root, names, sep } = folderParts(p);
  return names.length ? buildFolder(root, names.slice(0, -1), sep) : null;
}

/** `name` inside `dir`, with `dir`'s separator. */
export function childFolder(dir: string, name: string): string {
  const { root, names, sep } = folderParts(dir);
  return buildFolder(root, [...names, name], sep);
}

/** The steps from the root down to `p`, for a breadcrumb. */
export function folderCrumbs(p: string): { name: string; path: string }[] {
  const { root, names, sep } = folderParts(p);
  const out = root ? [{ name: folderName(root), path: root }] : [];
  names.forEach((n, i) => out.push({ name: n, path: buildFolder(root, names.slice(0, i + 1), sep) }));
  return out;
}

/** Whether two host paths name the same folder (Windows paths compare without case). */
export function sameFolder(a: string, b: string): boolean {
  const x = trimFolder(a);
  const y = trimFolder(b);
  const windows = (v: string) => /^([A-Za-z]:|\\\\)/.test(v) || (v.includes("\\") && !v.includes("/"));
  if (!windows(x) && !windows(y)) return x === y;
  const norm = (v: string) => v.replace(/\//g, "\\").toLowerCase();
  return norm(x) === norm(y);
}

/** `p` with the host user's home shortened to `~` (when that home is known). */
export function tildeFolder(p: string, home: string | null | undefined): string {
  if (!home) return p;
  const h = trimFolder(home);
  if (sameFolder(p, h)) return "~";
  const { names: hn } = folderParts(h);
  const { names: pn, sep } = folderParts(p);
  if (!hn.length || pn.length <= hn.length) return p;
  return sameFolder(buildFolder(folderParts(p).root, pn.slice(0, hn.length), sep), h) ? `~${sep}${pn.slice(hn.length).join(sep)}` : p;
}

/** The folders to offer first: recent ones, then those of the host's open sessions; no repeats. */
export function folderChoices(recent: string[], sessions: string[] = [], limit = 8): string[] {
  const out: string[] = [];
  for (const f of [...recent, ...sessions]) {
    const v = trimFolder(f);
    if (v && !out.some((o) => sameFolder(o, v))) out.push(v);
  }
  return out.slice(0, limit);
}

/** `folders` that match what was typed (name or path, without case); name matches first. */
export function matchFolders(folders: string[], query: string): string[] {
  const q = query.trim().toLowerCase();
  if (!q) return folders;
  const starts = (f: string) => folderName(f).toLowerCase().startsWith(q);
  return folders.filter((f) => f.toLowerCase().includes(q)).sort((a, b) => Number(starts(b)) - Number(starts(a)));
}

/** The subfolders in a `files_list` result, hidden ones (`.name`) only when asked for. */
export function listedFolders(entries: { name: string; kind: string }[], query = ""): string[] {
  const q = query.trim().toLowerCase();
  return entries
    .filter((e) => e.kind === "dir" || e.kind === "link")
    .map((e) => e.name)
    .filter((n) => (q.startsWith(".") || !n.startsWith(".")) && (!q || n.toLowerCase().includes(q)));
}

/** The host user's home as last seen by a listing of `~`, remembered per host. */
const HOME_KEY = "ai-remote.chat.home";

export function knownHome(host: string): string | null {
  try {
    return (JSON.parse(localStorage.getItem(HOME_KEY) ?? "{}") as Record<string, string>)[host] ?? null;
  } catch {
    return null;
  }
}

export function rememberHome(host: string, home: string) {
  try {
    const all = JSON.parse(localStorage.getItem(HOME_KEY) ?? "{}") as Record<string, string>;
    all[host] = home;
    localStorage.setItem(HOME_KEY, JSON.stringify(all));
  } catch {
    // Nothing to remember then.
  }
}

/** The host's sessions by folder (Orca's and Codex's "project → its threads"): the folder with the
 *  latest activity first, and inside each the latest session first. */
export function sessionsByFolder<S extends { cwd: string; last_activity: number }>(sessions: S[]): { folder: string; sessions: S[] }[] {
  const groups: { folder: string; sessions: S[] }[] = [];
  for (const s of [...sessions].sort((a, b) => b.last_activity - a.last_activity)) {
    const g = groups.find((x) => sameFolder(x.folder, s.cwd));
    if (g) g.sessions.push(s);
    else groups.push({ folder: trimFolder(s.cwd), sessions: [s] });
  }
  return groups;
}

/** How long ago `then` was, in the largest whole unit (for "3 min ago"). */
export function agoParts(then: number, now: number): { unit: "now" | "m" | "h" | "d"; n: number } {
  const m = Math.floor(Math.max(0, now - then) / 60000);
  if (m < 1) return { unit: "now", n: 0 };
  if (m < 60) return { unit: "m", n: m };
  if (m < 60 * 24) return { unit: "h", n: Math.floor(m / 60) };
  return { unit: "d", n: Math.floor(m / 1440) };
}
