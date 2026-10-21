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
  LocalHost,
  NetworkSettings,
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
const network: NetworkSettings = { port_mapping: false, locked: false, host_running: true };

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

let demoLoginItem = false;

const demoHostStatus: NonNullable<LocalHost["host"]> = {
  name: "home-laptop",
  device: "eid:4f2a9c1e77b0d3a8e51c0b6f2d9a7e43",
  version: "0.1.0",
  peers: 2,
  connected: ["laptop"],
  controllers: [{ name: "laptop", device: "eid:7c1d…", channels: ["events", "screen", "tools"] }],
  calls: [{ peer: "laptop", tool: "screenshot", person: false, since_ms: Date.now() - 400 }],
  last_call: null,
  driver: { state: "agent" },
  prompt_blocker: null,
  pid: 4242,
  background: true,
};

/** This computer as a host in the demo: running, one controller connected, one permission missing. */
const demoLocalHost: LocalHost = {
  host: { ...demoHostStatus },
  doctor: {
    overall: "fail",
    checked_in: "host",
    checks: [
      { id: "screen_recording", grade: "ok", summary: "macOS Screen Recording permission granted to this process" },
      { id: "accessibility", grade: "fail", summary: "macOS Accessibility permission missing: cannot control mouse and keyboard", fix: "enable the host executable in System Settings > Privacy & Security > Accessibility, restart the host, then run doctor again" },
      { id: "desktop", grade: "ok", summary: "desktop unlocked" },
      { id: "approval", grade: "ok", summary: "approval prompts can be shown on this desktop" },
      { id: "bypass", grade: "ok", summary: "no controller bypasses levels, policy and approval" },
      { id: "autostart", grade: "ok", summary: "LaunchAgent io.github.jia-ss.ai-remote.host is installed and running" },
      { id: "relay", grade: "ok", summary: "home relay reachable" },
    ],
  },
  profile: "/Users/me/Library/Application Support/ai-remote",
};

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
  async networkSettings() {
    await wait(80);
    return { ...network };
  },
  async setPortMapping(on) {
    await wait(300);
    network.port_mapping = on;
    return { ...network };
  },
  async localHost(doctor) {
    await wait(120);
    return { ...demoLocalHost, host: demoLocalHost.host && { ...demoLocalHost.host }, doctor: doctor ? demoLocalHost.doctor : null };
  },
  async localPairCode(level) {
    await wait(300);
    // A phone "scans" the demo code a while later, so the safety words can be reviewed too.
    demoPairedAt = Date.now() + 12_000;
    demoPairedLevel = level;
    const now = Math.floor(Date.now() / 1000);
    return {
      line: "mcpmesh-invite:DEMO" + level.toUpperCase() + Math.random().toString(36).slice(2, 10).toUpperCase(),
      expires_at: now + 86_400,
      level,
      code: { code: "14071145", grouped: "1407 1145", link: "ai-remote://pair?code=14071145", expires_at: now + 600 },
      qr: DEMO_QR,
    };
  },
  async localWithdrawCode() {
    demoPairedAt = 0;
  },
  async localPause(on) {
    await wait(150);
    if (demoLocalHost.host) demoLocalHost.host.driver = on ? { state: "paused", by: "host", since_ms: Date.now() } : { state: "agent" };
  },
  async localStop() {
    await wait(300);
    demoLocalHost.host = null;
  },
  async localStart() {
    await wait(800);
    demoLocalHost.host = { ...demoHostStatus };
  },
  async localRecent() {
    await wait(100);
    const now = Date.now();
    return [
      { ts: now - 9 * minute, kind: "session_start", peer: "laptop", client: "claude-code" },
      { ts: now - 8 * minute, kind: "decision", peer: "laptop", tool: "screenshot", decision: "allow" },
      { ts: now - 8 * minute + 400, kind: "outcome", peer: "laptop", tool: "screenshot", outcome: "ok" },
      { ts: now - 6 * minute, kind: "decision", peer: "laptop", tool: "shell", decision: "approved", by: "host", intent: "list the project folder" },
      { ts: now - 6 * minute + 900, kind: "outcome", peer: "laptop", tool: "shell", outcome: "ok" },
      { ts: now - 2 * minute, kind: "driver", state: "person", device: "laptop", by: "laptop" },
      ...(demoPairedAt && now >= demoPairedAt
        ? [{ ts: demoPairedAt, kind: "paired", peer: "iphone-15", sas: "harbor-velvet-orbit", level: demoPairedLevel }]
        : []),
    ];
  },
  async localForget() {
    await wait(200);
  },
  async protectWindow() {},
  async openSystemSettings() {},
  async revealProfile() {},
  async setUiLanguage() {},
  async appLoginItem() {
    return demoLoginItem;
  },
  async setAppLoginItem(on) {
    demoLoginItem = on;
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

let demoPairedAt = 0;
let demoPairedLevel = "control";

/** The QR code of the demo pair code's link (made by the host's `pair_qr::svg_path`). */
const DEMO_QR = { size: 29, path: "M0 0h7v1h-7zM8 0h1v1h-1zM10 0h3v1h-3zM14 0h5v1h-5zM20 0h1v1h-1zM22 0h7v1h-7zM0 1h1v1h-1zM6 1h1v1h-1zM11 1h3v1h-3zM18 1h2v1h-2zM22 1h1v1h-1zM28 1h1v1h-1zM0 2h1v1h-1zM2 2h3v1h-3zM6 2h1v1h-1zM8 2h2v1h-2zM11 2h1v1h-1zM15 2h1v1h-1zM17 2h1v1h-1zM19 2h2v1h-2zM22 2h1v1h-1zM24 2h3v1h-3zM28 2h1v1h-1zM0 3h1v1h-1zM2 3h3v1h-3zM6 3h1v1h-1zM8 3h5v1h-5zM16 3h1v1h-1zM19 3h1v1h-1zM22 3h1v1h-1zM24 3h3v1h-3zM28 3h1v1h-1zM0 4h1v1h-1zM2 4h3v1h-3zM6 4h1v1h-1zM9 4h3v1h-3zM16 4h1v1h-1zM18 4h1v1h-1zM20 4h1v1h-1zM22 4h1v1h-1zM24 4h3v1h-3zM28 4h1v1h-1zM0 5h1v1h-1zM6 5h1v1h-1zM8 5h2v1h-2zM12 5h1v1h-1zM14 5h1v1h-1zM19 5h2v1h-2zM22 5h1v1h-1zM28 5h1v1h-1zM0 6h7v1h-7zM8 6h1v1h-1zM10 6h1v1h-1zM12 6h1v1h-1zM14 6h1v1h-1zM16 6h1v1h-1zM18 6h1v1h-1zM20 6h1v1h-1zM22 6h7v1h-7zM8 7h3v1h-3zM12 7h1v1h-1zM14 7h1v1h-1zM16 7h2v1h-2zM19 7h1v1h-1zM1 8h1v1h-1zM3 8h1v1h-1zM5 8h4v1h-4zM11 8h2v1h-2zM14 8h1v1h-1zM16 8h8v1h-8zM25 8h2v1h-2zM28 8h1v1h-1zM0 9h1v1h-1zM2 9h1v1h-1zM4 9h1v1h-1zM8 9h1v1h-1zM12 9h1v1h-1zM14 9h2v1h-2zM19 9h2v1h-2zM23 9h1v1h-1zM25 9h1v1h-1zM27 9h2v1h-2zM0 10h1v1h-1zM3 10h1v1h-1zM6 10h1v1h-1zM8 10h1v1h-1zM10 10h1v1h-1zM12 10h4v1h-4zM17 10h1v1h-1zM21 10h4v1h-4zM27 10h1v1h-1zM1 11h2v1h-2zM5 11h1v1h-1zM9 11h1v1h-1zM11 11h1v1h-1zM13 11h1v1h-1zM15 11h2v1h-2zM18 11h3v1h-3zM23 11h2v1h-2zM26 11h1v1h-1zM0 12h7v1h-7zM9 12h2v1h-2zM12 12h1v1h-1zM15 12h3v1h-3zM21 12h2v1h-2zM28 12h1v1h-1zM1 13h5v1h-5zM8 13h1v1h-1zM14 13h1v1h-1zM16 13h1v1h-1zM18 13h2v1h-2zM21 13h1v1h-1zM26 13h2v1h-2zM1 14h1v1h-1zM4 14h3v1h-3zM9 14h1v1h-1zM11 14h1v1h-1zM14 14h1v1h-1zM16 14h1v1h-1zM20 14h1v1h-1zM22 14h1v1h-1zM24 14h2v1h-2zM27 14h2v1h-2zM2 15h1v1h-1zM7 15h1v1h-1zM13 15h1v1h-1zM15 15h2v1h-2zM19 15h3v1h-3zM25 15h1v1h-1zM28 15h1v1h-1zM3 16h1v1h-1zM5 16h2v1h-2zM9 16h1v1h-1zM16 16h1v1h-1zM19 16h1v1h-1zM23 16h1v1h-1zM25 16h1v1h-1zM27 16h2v1h-2zM1 17h1v1h-1zM5 17h1v1h-1zM7 17h1v1h-1zM10 17h6v1h-6zM20 17h3v1h-3zM27 17h2v1h-2zM0 18h1v1h-1zM4 18h1v1h-1zM6 18h1v1h-1zM11 18h1v1h-1zM13 18h1v1h-1zM18 18h4v1h-4zM23 18h3v1h-3zM27 18h2v1h-2zM2 19h1v1h-1zM4 19h2v1h-2zM7 19h1v1h-1zM9 19h3v1h-3zM14 19h4v1h-4zM20 19h1v1h-1zM25 19h1v1h-1zM27 19h1v1h-1zM0 20h1v1h-1zM2 20h3v1h-3zM6 20h1v1h-1zM9 20h2v1h-2zM12 20h1v1h-1zM14 20h1v1h-1zM19 20h6v1h-6zM26 20h3v1h-3zM8 21h1v1h-1zM10 21h1v1h-1zM14 21h1v1h-1zM16 21h2v1h-2zM19 21h2v1h-2zM24 21h1v1h-1zM26 21h1v1h-1zM28 21h1v1h-1zM0 22h7v1h-7zM8 22h1v1h-1zM10 22h3v1h-3zM16 22h1v1h-1zM18 22h1v1h-1zM20 22h1v1h-1zM22 22h1v1h-1zM24 22h1v1h-1zM26 22h1v1h-1zM0 23h1v1h-1zM6 23h1v1h-1zM8 23h6v1h-6zM15 23h1v1h-1zM18 23h1v1h-1zM20 23h1v1h-1zM24 23h1v1h-1zM26 23h3v1h-3zM0 24h1v1h-1zM2 24h3v1h-3zM6 24h1v1h-1zM9 24h4v1h-4zM14 24h3v1h-3zM18 24h1v1h-1zM20 24h6v1h-6zM28 24h1v1h-1zM0 25h1v1h-1zM2 25h3v1h-3zM6 25h1v1h-1zM8 25h1v1h-1zM13 25h1v1h-1zM15 25h1v1h-1zM17 25h2v1h-2zM22 25h4v1h-4zM28 25h1v1h-1zM0 26h1v1h-1zM2 26h3v1h-3zM6 26h1v1h-1zM9 26h2v1h-2zM12 26h8v1h-8zM21 26h1v1h-1zM24 26h1v1h-1zM28 26h1v1h-1zM0 27h1v1h-1zM6 27h1v1h-1zM8 27h1v1h-1zM11 27h1v1h-1zM13 27h2v1h-2zM16 27h4v1h-4zM21 27h1v1h-1zM23 27h2v1h-2zM27 27h1v1h-1zM0 28h7v1h-7zM9 28h4v1h-4zM14 28h2v1h-2zM17 28h1v1h-1zM19 28h1v1h-1zM24 28h2v1h-2zM27 28h1v1h-1z" };
