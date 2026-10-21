// A demo backend for the page outside the app (design review, screenshots, browser preview): four
// sample computers and a scripted session that matches the design's sample content.

import type { Backend, ToolResult } from "./api";
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
  async callTool(host, tool): Promise<ToolResult> {
    reachable(host);
    await wait(250);
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
  async setLocalBypass(peer, on) {
    await wait(200);
    const c = localControllers.find((x) => x.name === peer);
    if (c) Object.assign(c, { bypass: on, since_ms: on ? Date.now() : null, until_ms: null });
  },
  async localHost(doctor) {
    await wait(120);
    return { ...demoLocalHost, host: demoLocalHost.host && { ...demoLocalHost.host }, doctor: doctor ? demoLocalHost.doctor : null };
  },
  async localPairCode(level) {
    await wait(300);
    return { line: "ai-remote-ticket-demo-" + level + "-" + Math.random().toString(36).slice(2, 10), expires_at: Math.floor(Date.now() / 1000) + 86_400, level };
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
