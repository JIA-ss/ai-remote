// A demo backend for the page outside the app (design review, screenshots, browser preview): four
// sample computers and a scripted session that matches the design's sample content.

import type { Backend, ToolResult } from "./api";
import type { ChatEvent, ChatHello, ChatState, RuntimesReport, SessionInfo } from "./chat";
import type {
  ConnectedAgent,
  Driver,
  FeedEvent,
  Host,
  HostAccess,
  HostTool,
  Level,
  LocalController,
  PendingApproval,
  RawPending,
} from "./types";

const minute = 60_000;

const hosts: Host[] = [
  { name: "office-pc", device: "eid:7f3a…", levelAtPairing: "admin", pairedAt: 1_727_654_400, path: "direct" },
  { name: "lab-pc", device: "eid:21c9…", levelAtPairing: "control", pairedAt: 1_727_740_800, path: "relay", relay: "aps1-1.relay.n0.iroh.link", relayN0: true, p2pObstacle: "remote" },
  { name: "studio-mac", device: "eid:b04e…", levelAtPairing: "control", pairedAt: 1_728_000_000, path: null },
  { name: "render-box", device: "eid:e88d…", levelAtPairing: "observe", pairedAt: 1_728_086_400, path: null },
];

const offline = new Set(["render-box"]);
const drivers = new Map<string, Driver>();

type Listener = (e: FeedEvent) => void;
const listeners = new Map<string, Set<Listener>>();
const history = new Map<string, FeedEvent[]>();
const pending = new Map<string, RawPending[]>();

function emit(host: string, e: FeedEvent) {
  history.set(host, [...(history.get(host) ?? []), e]);
  listeners.get(host)?.forEach((l) => l(e));
}

/** The office-pc story up to the point where the agent waits for you. */
function seedOffice() {
  const start = Date.now() - 2 * minute - 9_000;
  const s = "s-7f3a";
  const peer = "home-laptop";
  const ev: FeedEvent[] = [
    { kind: "session_start", ts: start, session: s, peer, client: "agent", agent: "claude-code" },
    {
      kind: "task",
      ts: start + 400,
      session: s,
      peer,
      task: "Export the Q3 report as a PDF, delete the old drafts, and share the report with Finance.",
    },
  ];
  const call = (at: number, tool: string, hash: string, intent: string, args: string, ms: number) => {
    ev.push({ kind: "decision", ts: start + at, session: s, peer, tool, decision: "allow", intent, args_hash: hash, args_preview: args });
    ev.push({ kind: "outcome", ts: start + at + ms, session: s, peer, tool, args_hash: hash, outcome: "ok", duration_ms: ms });
  };
  call(1_200, "list_windows", "a1", "Find the Q3 report window", '{"intent":"Find the Q3 report window"}', 180);
  call(4_800, "ui_tree", "a2", "Read the Q3 report window", '{"window":"Q3 report","intent":"Read the Q3 report window"}', 420);
  call(19_000, "mouse", "a3", "Click Export PDF", '{"action":"click","element":"e4","intent":"Click Export PDF"}', 260);
  call(31_000, "screenshot", "a4", "Check that the PDF was saved", '{"intent":"Check that the PDF was saved"}', 610);
  call(58_000, "shell", "a5", "List the old drafts", '{"command":"Get-ChildItem C:\\\\Reports\\\\Q3\\\\drafts","intent":"List the old drafts"}', 900);
  history.set("office-pc", ev);
  const ask: RawPending = {
    id: "3f9c2a71d0b84e55a6c1e2f3b4d5c6a7",
    session: s,
    peer,
    tool: "shell",
    intent: "You asked me to remove the drafts once the PDF is saved.",
    args_hash: "a6",
    args_preview: '{"command":"Remove-Item C:\\\\Reports\\\\Q3\\\\drafts\\\\* -Recurse"}',
    expires_ms: Date.now() + 4 * minute + 52_000,
  };
  emit("office-pc", { kind: "approval", ts: Date.now() - 8_000, remote: true, ...ask });
  pending.set("office-pc", [ask]);
}

function seedLab() {
  const start = Date.now() - 6 * minute;
  const s = "s-21c9";
  const peer = "ci-runner";
  history.set("lab-pc", [
    { kind: "session_start", ts: start, session: s, peer, client: "agent", agent: "codex" },
    { kind: "decision", ts: start + 2_000, session: s, peer, tool: "jobs_start", decision: "allow", intent: "Run the nightly build", args_hash: "b1", args_preview: '{"command":"./build.ps1"}' },
    { kind: "outcome", ts: start + 2_400, session: s, peer, tool: "jobs_start", args_hash: "b1", outcome: "ok", duration_ms: 400 },
    { kind: "decision", ts: Date.now() - 4_000, session: s, peer, tool: "ui_tree", decision: "allow", intent: "Reading build.log in Notepad", args_hash: "b2", args_preview: '{"window":"build.log - Notepad"}' },
  ]);
}

seedOffice();
seedLab();

const toPending = (p: RawPending): PendingApproval => ({
  id: p.id,
  peer: p.peer,
  tool: p.tool,
  intent: p.intent ?? null,
  argsPreview: p.args_preview,
  expiresMs: p.expires_ms,
});

/** Controllers paired with this computer, as its host would list them. */
const localControllers: LocalController[] = [
  { name: "work-laptop", device: "eid:7f3a91c2", level: "admin", revoked: false, bypass: false, since_ms: null, until_ms: null },
  { name: "iphone", device: "eid:2b8e10d4", level: "observe", revoked: false, bypass: false, since_ms: null, until_ms: null },
];

function reachable(host: string) {
  if (offline.has(host)) throw new Error(`host '${host}' is offline or unreachable`);
}

function resolve(host: string, id: string, verdict: "approved" | "denied", note?: string) {
  const list = pending.get(host) ?? [];
  const p = list.find((x) => x.id === id);
  if (!p) throw new Error(`nothing with id '${id}' is waiting on host '${host}'`);
  pending.set(host, list.filter((x) => x.id !== id));
  const now = Date.now();
  emit(host, { kind: "approval_resolved", ts: now, id, verdict, by: "home-laptop", note, waited_ms: 9_000 });
  emit(host, {
    kind: "decision",
    ts: now,
    session: p.session,
    peer: p.peer,
    tool: p.tool,
    decision: verdict,
    intent: p.intent ?? undefined,
    args_hash: p.args_hash,
    args_preview: p.args_preview,
    approval: id,
    by: "home-laptop",
  });
  if (verdict === "approved") {
    setTimeout(
      () => emit(host, { kind: "outcome", ts: Date.now(), session: p.session, peer: p.peer, tool: p.tool, args_hash: p.args_hash, outcome: "ok", duration_ms: 840 }),
      840,
    );
  }
}

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

const demoAgents: ConnectedAgent[] = [
  { id: 1, client: "claude-code", since_ms: Date.now() - 40 * minute, last_ms: Date.now() - 2 * minute, calls: 13 },
];

export const demoBackend: Backend = {
  kind: "demo",
  async status() {
    const agents = demoAgents.map((a) => ({ ...a }));
    return { device: "eid:4f2a9c1e77b0d3a8e51c0b6f2d9a7e43", version: "0.1.0", name: "home-laptop", agents,
      mcp: { command: "ai-remote", args: ["mcp"] } };
  },
  async openAppearanceSettings() {},
  async fixFirewall() {},
  async disconnectAgent(id) {
    const i = demoAgents.findIndex((a) => a.id === id);
    if (i >= 0) demoAgents.splice(i, 1);
  },
  async hosts() {
    // `?empty` shows the first run.
    if (new URLSearchParams(location.search).has("empty")) return [];
    return hosts.map((h) => ({ ...h }));
  },
  async pair(ticket) {
    await wait(600);
    if (!ticket.trim()) throw new Error("that is not a pairing ticket");
    return { peer: "new-pc", sas: "amount-bubble-acquire", level: "control" };
  },
  async unpair(host) {
    const i = hosts.findIndex((h) => h.name === host);
    if (i >= 0) hosts.splice(i, 1);
  },
  async callTool(host, tool, args = {}): Promise<ToolResult> {
    reachable(host);
    await wait(250);
    const chat = demoChatTool(host, tool, args);
    if (chat) return chat;
    if (tool === "screenshot") {
      return { content: [{ type: "image", mimeType: "image/svg+xml", data: btoa(demoScreen()) }] };
    }
    return { content: [{ type: "text", text: `${tool}: ok (demo)` }] };
  },
  async subscribe(host, onEvent) {
    reachable(host);
    onEvent({ kind: "hello", host, version: "demo", os: host === "studio-mac" ? "macos" : "windows", level: hosts.find((h) => h.name === host)?.levelAtPairing,
      pending: pending.get(host) ?? [], driver: drivers.get(host) ?? { state: "agent" } });
    for (const e of history.get(host) ?? []) onEvent(e);
    const set = listeners.get(host) ?? new Set<(e: FeedEvent) => void>();
    set.add(onEvent);
    listeners.set(host, set);
    return () => set.delete(onEvent);
  },
  async pending(host) {
    reachable(host);
    return (pending.get(host) ?? []).map(toPending);
  },
  async approve(host, id) {
    await wait(700); // stands in for the Windows Hello check
    resolve(host, id, "approved");
  },
  async deny(host, id, note) {
    resolve(host, id, "denied", note);
  },
  async approverStatus(host) {
    reachable(host);
    return host === "office-pc" ? true : null;
  },
  async localControllers() {
    await wait(120);
    return localControllers.map((c) => ({ ...c }));
  },
  async localChat() {
    await wait(80);
    return { enabled: demoChatEnabled, live: 1, sessions: [] };
  },
  async setLocalChat(on) {
    await wait(150);
    demoChatEnabled = on;
  },
  async openChat(host, ticket, since, onEvent, onClosed) {
    reachable(host);
    await wait(200);
    const s = demoSessions.find((x) => demoTickets.get(ticket) === x.id);
    if (!s) throw new Error("unknown or already used chat ticket; ask for a new one");
    demoTickets.delete(ticket);
    const log = demoLogs.get(s.id) ?? [];
    const hello: ChatHello = { session: s.id, role: "rw", runtime: { id: s.runtime, name: s.name }, cwd: s.cwd, state: s.state,
      seq: log.length, caps: { interrupt: true, resume: true, partial: true, thinking: true, questions: false }, host };
    for (const e of log) if (e.seq > since) onEvent(e);
    const listener = (e: ChatEvent) => onEvent(e);
    demoChatListeners.add(listener);
    return {
      hello,
      send: async (command) => demoChatCommand(s, command),
      close: () => {
        demoChatListeners.delete(listener);
        onClosed();
      },
    };
  },
  async setLocalBypass(peer, on) {
    await wait(200);
    const c = localControllers.find((x) => x.name === peer);
    if (c) Object.assign(c, { bypass: on, since_ms: on ? Date.now() : null, until_ms: null });
  },
  async enrollApprover(host) {
    reachable(host);
    await wait(900);
  },
  async setTurn(host, action, opts = {}) {
    reachable(host);
    const now = Date.now();
    const driver: Driver =
      action === "take_over"
        ? { state: "person", device: "home-laptop", since_ms: now, agent_may_watch: opts.agentMayWatch ?? false }
        : action === "pause"
          ? { state: "paused", by: "home-laptop", since_ms: now }
          : { state: "agent" };
    drivers.set(host, driver);
    emit(host, { kind: "driver", ts: now, by: "home-laptop", note: opts.note, ...driver });
    return driver;
  },
  async tellAgent(host, text) {
    reachable(host);
    emit(host, { kind: "note", ts: Date.now(), from: "home-laptop", text });
  },
  async access(host) {
    reachable(host);
    await wait(150);
    const h = hosts.find((x) => x.name === host);
    const level = h?.levelAtPairing ?? null;
    const tool = (name: string, level: Level, action: HostTool["action"], description: string): HostTool => ({
      name,
      level,
      action,
      description,
    });
    return {
      level,
      you: "home-laptop",
      rules: [{ tool: "git_*", action: "allow", scope: "you" }],
      devices: [
        { name: "home-laptop", level, revoked: false, you: true },
        ...(level === "admin" ? [{ name: "iphone", level: "control" as Level, revoked: false, you: false }] : []),
      ],
      builtin: [
        tool("system_info", "observe", "allow", "Machine name, OS, screens and whether the desktop can be used."),
        tool("screenshot", "observe", "allow", "A picture of one screen."),
        tool("list_windows", "observe", "allow", "Open windows with their titles."),
        tool("ui_tree", "observe", "allow", "A native accessibility tree with bounded, session-bound references."),
        tool("ui_read", "observe", "allow", "Read one element from the current accessibility snapshot."),
        tool("ui_action", "control", "allow", "Act on one element from the current accessibility snapshot."),
        tool("mouse", "control", "allow", "Move, click and drag."),
        tool("keyboard", "control", "allow", "Type text and press keys."),
        tool("clipboard_read", "control", "allow", "Read the clipboard."),
        tool("shell", "admin", "ask", "Run a PowerShell command."),
        tool("jobs_start", "admin", "ask", "Start a long-running command in the background."),
      ],
      mounts: [
        {
          name: "git",
          needs_desktop: false,
          lazy: true,
          tools: [
            tool("git_status", "admin", "allow", "Show the working tree status."),
            tool("git_log", "admin", "allow", "Show commit history."),
            tool("git_diff", "admin", "allow", "Show changes."),
            tool("git_commit", "admin", "allow", "Record staged changes."),
            tool("git_push", "admin", "ask", "Send commits to a remote."),
          ],
        },
      ],
    } satisfies HostAccess;
  },
  async lowerLevel(host, level) {
    reachable(host);
    const h = hosts.find((x) => x.name === host);
    if (h) h.levelAtPairing = level;
  },
  async watchScreen(host) {
    reachable(host);
    return null;
  },
  async openInput(host) {
    reachable(host);
    throw new Error("live input needs the app");
  },
  async clipboardText() {
    return null;
  },
  async setClipboardText() {},
  async clipboardFiles() {
    return [];
  },
  async pushFile() {
    throw new Error("file transfers need the app");
  },
  async pullFile() {
    throw new Error("file transfers need the app");
  },
};

/** The demo's stand-in for a live view: a still picture of office-pc's screen. */
export function demoScreenUrl() {
  return `data:image/svg+xml;base64,${btoa(demoScreen())}`;
}

/** A stand-in for office-pc's screen: a report window on a quiet Windows desktop. */
function demoScreen() {
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1600 1000" width="1600" height="1000">
<defs><linearGradient id="w" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#9DB3C8"/><stop offset="1" stop-color="#C9D3DC"/></linearGradient></defs>
<rect width="1600" height="1000" fill="url(#w)"/>
<rect x="240" y="110" width="1120" height="720" rx="8" fill="#FFFFFF" stroke="#D5DAE0"/>
<rect x="240" y="110" width="1120" height="44" rx="8" fill="#F3F4F6"/>
<text x="268" y="139" font-family="Segoe UI, sans-serif" font-size="15" fill="#1F2328">Q3 report</text>
<rect x="272" y="178" width="96" height="30" rx="4" fill="#F3F4F6" stroke="#D5DAE0"/><text x="290" y="198" font-family="Segoe UI, sans-serif" font-size="13" fill="#1F2328">File</text>
<rect x="1084" y="178" width="120" height="30" rx="4" fill="#F3F4F6" stroke="#D5DAE0"/><text x="1102" y="198" font-family="Segoe UI, sans-serif" font-size="13" fill="#1F2328">Export PDF</text>
<rect x="1214" y="178" width="120" height="30" rx="4" fill="#F3F4F6" stroke="#D5DAE0"/><text x="1252" y="198" font-family="Segoe UI, sans-serif" font-size="13" fill="#1F2328">Share</text>
<text x="300" y="268" font-family="Segoe UI, sans-serif" font-size="30" font-weight="600" fill="#1F2328">Q3 revenue review</text>
${[0, 1, 2, 3, 4, 5].map((i) => `<rect x="300" y="${310 + i * 34}" width="${640 - i * 50}" height="12" rx="6" fill="#E4E7EB"/>`).join("")}
${[0, 1, 2, 3, 4].map((i) => `<rect x="${1000 + i * 64}" y="${640 - [120, 180, 150, 240, 210][i]}" width="40" height="${[120, 180, 150, 240, 210][i]}" rx="4" fill="#B9C4CF"/>`).join("")}
<rect x="0" y="952" width="1600" height="48" fill="#E9EDF1" fill-opacity="0.92"/>
</svg>`;
}


// Coding agents on office-pc: what is installed, one session with a finished turn, and a scripted
// reply to whatever is sent.

let demoChatEnabled = true;
const demoTickets = new Map<string, string>();
const demoChatListeners = new Set<(e: ChatEvent) => void>();

const demoRuntimes: RuntimesReport = {
  host: { can_start: true, reason: null, enabled: true, max_live: 4 },
  runtimes: [
    { id: "claude", name: "Claude Code", base: "claude", custom: false, protocol: "claude-stream-json", verdict: "ok", version: "2.1.174",
      path: "C:\\Users\\me\\.local\\bin\\claude.exe", login: { state: "logged_in", method: "claude.ai", plan: "max" },
      caps: { interrupt: true, resume: true, partial: true, thinking: true, questions: false } },
    { id: "codex", name: "Codex", base: "codex", custom: false, protocol: "codex-app-server", verdict: "ok", version: "0.163.0",
      path: "C:\\Users\\me\\AppData\\Roaming\\npm\\codex.cmd", login: { state: "logged_in", method: "chatgpt" },
      caps: { interrupt: true, resume: true, partial: true, thinking: true, questions: true } },
    { id: "opencode", name: "OpenCode", base: "opencode", custom: false, protocol: "acp", verdict: "ok", version: "1.2.4",
      path: "C:\\Users\\me\\AppData\\Roaming\\npm\\opencode.cmd", login: null,
      caps: { interrupt: true, resume: true, partial: true, thinking: true, questions: false } },
    { id: "gemini", name: "Gemini CLI", base: "gemini", custom: false, protocol: "acp", verdict: "missing",
      caps: { interrupt: true, resume: true, partial: true, thinking: true, questions: false } },
    { id: "qwen", name: "Qwen Code", base: "qwen", custom: false, protocol: "acp", verdict: "missing",
      caps: { interrupt: true, resume: true, partial: true, thinking: true, questions: false } },
  ],
};

const t0 = Date.now() - 6 * minute;
const demoSessions: SessionInfo[] = [
  { id: "c3f9a1e7b2c4d", runtime: "claude", name: "Claude Code", cwd: "D:\\work\\report-tool", state: "idle", by: { device: "home-laptop" },
    started_at: t0, turns: 1, last_activity: t0 + 40_000, seq: 0 },
];

function demoLog(): ChatEvent[] {
  const evs: Array<Record<string, unknown>> = [
    { t: "state", state: "running", queued: [] },
    { t: "user", input_id: "u1", by: { device: "home-laptop" }, text: "The Q3 export fails with a date error. Find out why and fix it." },
    { t: "turn_start" },
    { t: "message", id: "m1", text: "Let me look at the export code and reproduce the error first.", delta: false },
    { t: "tool_call", call_id: "k1", title: "Bash python export.py --quarter Q3", kind: "execute", status: "in_progress", input: { command: "python export.py --quarter Q3" } },
    { t: "tool_update", call_id: "k1", status: "failed", content: [{ type: "text", text: "ValueError: time data '2026-09-31' does not match format '%Y-%m-%d'\n(exit code 1)" }] },
    { t: "thought", id: "r1", text: "September has 30 days; the quarter end is computed as month*3 and day 31.", delta: false },
    { t: "tool_call", call_id: "k2", title: "Read D:\\work\\report-tool\\export.py", kind: "read", status: "completed", locations: ["D:\\work\\report-tool\\export.py"] },
    { t: "tool_call", call_id: "k3", title: "Edit D:\\work\\report-tool\\export.py", kind: "edit", status: "completed", content: [
      { type: "diff", path: "D:\\work\\report-tool\\export.py", old: "    end = date(year, quarter * 3, 31)", new: "    last = calendar.monthrange(year, quarter * 3)[1]\n    end = date(year, quarter * 3, last)" }] },
    { t: "tool_call", call_id: "k4", title: "Bash python export.py --quarter Q3", kind: "execute", status: "in_progress" },
    { t: "tool_update", call_id: "k4", status: "completed", content: [{ type: "text", text: "wrote out/Q3.pdf (14 pages)" }] },
    { t: "message", id: "m2", text: "Fixed. The quarter end was always day 31, which does not exist for September (or June). `export.py` now takes the month's real last day from `calendar.monthrange`, and the Q3 export runs: `out/Q3.pdf`, 14 pages.", delta: false },
    { t: "usage", input_tokens: 18_400, output_tokens: 920 },
    { t: "turn_end", status: "completed", final_text: "Fixed.", duration_ms: 38_200 },
    { t: "state", state: "idle", queued: [] },
  ];
  return evs.map((e, i) => ({ seq: i + 1, ts: t0 + i * 2600, turn: i >= 2 ? 1 : 0, ...e }) as unknown as ChatEvent);
}
const demoLogs = new Map<string, ChatEvent[]>([["c3f9a1e7b2c4d", demoLog()]]);

function push(s: SessionInfo, e: Record<string, unknown>) {
  const log = demoLogs.get(s.id) ?? [];
  const ev = { seq: log.length + 1, ts: Date.now(), turn: s.turns, ...e } as unknown as ChatEvent;
  log.push(ev);
  demoLogs.set(s.id, log);
  s.seq = ev.seq;
  for (const l of demoChatListeners) l(ev);
}

function setState(s: SessionInfo, state: ChatState) {
  s.state = state;
  push(s, { t: "state", state, queued: [] });
}

async function demoReply(s: SessionInfo, text: string) {
  setState(s, "running");
  push(s, { t: "user", input_id: `u${s.turns + 1}`, by: { device: "home-laptop" }, text });
  s.turns += 1;
  push(s, { t: "turn_start" });
  await wait(500);
  push(s, { t: "tool_call", call_id: `k${s.seq}`, title: "Bash python -m pytest -q", kind: "execute", status: "in_progress" });
  await wait(1400);
  if (s.state !== "running") return;
  push(s, { t: "tool_update", call_id: `k${s.seq - 1}`, status: "completed", content: [{ type: "text", text: "12 passed in 1.84s" }] });
  const words = "All 12 tests pass, including the new one for quarter ends.".split(" ");
  for (const w of words) {
    await wait(70);
    if (s.state !== "running") return;
    push(s, { t: "message", id: `m${s.turns}`, text: `${w} `, delta: true });
  }
  push(s, { t: "turn_end", status: "completed", duration_ms: 2600 });
  setState(s, "idle");
}

async function demoChatCommand(s: SessionInfo, c: Record<string, unknown>) {
  const input = c.input as { text?: string } | undefined;
  if (input?.text) void demoReply(s, input.text);
  if (c.interrupt && s.state === "running") {
    setState(s, "interrupting");
    await wait(300);
    push(s, { t: "turn_end", status: "interrupted", duration_ms: 900 });
    setState(s, "idle");
  }
  if (c.end) setState(s, "ended");
}

function demoChatTool(host: string, tool: string, args: Record<string, unknown>): ToolResult | null {
  if (!tool.startsWith("chat_")) return null;
  const level = hosts.find((h) => h.name === host)?.levelAtPairing;
  if (level !== "admin") {
    return { isError: true, content: [{ type: "text", text: `'${tool}' requires level 'admin', but this device has '${level}'.` }] };
  }
  const ok = (v: unknown): ToolResult => ({ content: [{ type: "text", text: JSON.stringify(v) }], structuredContent: v });
  const ticket = (id: string) => {
    const tk = `t${Math.random().toString(16).slice(2)}`;
    demoTickets.set(tk, id);
    return tk;
  };
  switch (tool) {
    case "chat_runtimes":
      return ok({ ...demoRuntimes, host: { ...demoRuntimes.host, enabled: demoChatEnabled } });
    case "chat_sessions":
      return ok({ sessions: demoSessions, live: demoSessions.filter((s) => s.state !== "ended" && s.state !== "parked").length });
    case "chat_start": {
      if (!demoChatEnabled) return { isError: true, content: [{ type: "text", text: "coding agents are switched off on this host" }] };
      const rt = demoRuntimes.runtimes.find((r) => r.id === args.runtime);
      const s: SessionInfo = { id: `c${Math.random().toString(16).slice(2, 14)}`, runtime: String(args.runtime), name: rt?.name ?? String(args.runtime),
        cwd: String(args.cwd), state: "idle", by: { device: "home-laptop" }, started_at: Date.now(), turns: 0, last_activity: Date.now(), seq: 0 };
      demoSessions.unshift(s);
      demoLogs.set(s.id, []);
      if (typeof args.prompt === "string" && args.prompt) setTimeout(() => void demoReply(s, args.prompt as string), 600);
      return ok({ ...s, session: s.id, ticket: ticket(s.id), role: "rw", expires_in_secs: 120 });
    }
    case "chat_attach": {
      const s = demoSessions.find((x) => x.id === args.session);
      if (!s) return { isError: true, content: [{ type: "text", text: `no chat session '${String(args.session)}'` }] };
      return ok({ ...s, session: s.id, ticket: ticket(s.id), role: "rw", expires_in_secs: 120 });
    }
    case "chat_end": {
      const s = demoSessions.find((x) => x.id === args.session);
      if (s) setState(s, "ended");
      return ok({ session: args.session, ending: true });
    }
    case "chat_interrupt": {
      const s = demoSessions.find((x) => x.id === args.session);
      if (s) void demoChatCommand(s, { interrupt: {} });
      return ok({ session: args.session, interrupting: true });
    }
    default:
      return null;
  }
}
