// The agents workspace's data (docs/agent-workspace.zh-CN.md §2.2): the sessions of every computer,
// polled with `chat_sessions` and merged into one list, what is unread here, pins / archive / titles
// through `chat_update` (with what an older host allows instead), and the sidebar's grouping, sorting,
// search and status dots. The behavior follows the clean-room specs in ~/ar-refs/spec (SB = sidebar,
// NS = new session page); rule ids are quoted where a number comes from them.
//
// Everything here is plain code over plain data, and the only import is lib/chat.ts's path helpers,
// so tools/workspace.test.mjs runs it in node. The store reaches hosts only through the `call` it is
// given (ui/workspace/useWorkspace.ts gives it the app's tool call).

import { folderName, parentFolder, sameFolder, tildeFolder, trimFolder, type ChatState } from "./chat";

// ---------------------------------------------------------------------------------------------
// Session metadata

/** What waits for a person in a session (an approval, a question, a plan to review). */
export type WsPending = "permission" | "question" | "plan";
/** How the session's last turn ended. */
export type WsLastStatus = "completed" | "failed" | "interrupted";

/**
 * One session as `chat_sessions` reports it. Everything after `seq` came with the workspace and is
 * missing from older hosts (they are then shown with what they have, see `displayTitle` / `dotOf`).
 */
export interface WsMeta {
  id: string;
  runtime: string;
  name: string;
  cwd: string;
  model?: string | null;
  version?: string | null;
  state: ChatState;
  by?: { device: string; principal?: string | null };
  started_at: number;
  turns: number;
  last_activity: number;
  ended_at?: number | null;
  end_reason?: string | null;
  /** The last event's number (every host has it; `last_seq` is the same, kept with the metadata). */
  seq?: number;
  title?: string | null;
  title_source?: "auto" | "agent" | "user" | null;
  first_text?: string | null;
  last_status?: WsLastStatus | null;
  last_reason?: string | null;
  pending?: WsPending | null;
  last_seq?: number;
  branch?: string | null;
  pinned?: boolean;
  archived?: boolean;
  permission_mode?: string | null;
}

/** A session with the computer it is on. */
export interface WsItem {
  host: string;
  /** `host/id`: sessions are only unique per host. */
  key: string;
  meta: WsMeta;
  /** The computer is this one (its host runs here): no computer mark, no "· name". */
  local: boolean;
  /** The computer is online (null: not known yet). Sessions of an offline one are the last ones seen. */
  online: boolean | null;
}

export const sessionKey = (host: string, id: string) => `${host}/${id}`;

/** A session's metadata as received, with numbers that are numbers (a broken entry is dropped). */
export function normalizeMeta(raw: unknown): WsMeta | null {
  if (!raw || typeof raw !== "object") return null;
  const m = raw as Record<string, unknown>;
  if (typeof m.id !== "string" || !m.id) return null;
  const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : 0);
  return {
    ...(m as unknown as WsMeta),
    runtime: typeof m.runtime === "string" ? m.runtime : "",
    name: typeof m.name === "string" ? m.name : "",
    cwd: typeof m.cwd === "string" ? m.cwd : "",
    state: (typeof m.state === "string" ? m.state : "idle") as ChatState,
    started_at: num(m.started_at),
    turns: num(m.turns),
    last_activity: num(m.last_activity) || num(m.started_at),
  };
}

/** The session's last event number: `last_seq`, or `seq` from a host that has no `last_seq`. */
export function lastSeq(m: WsMeta): number {
  return m.last_seq ?? m.seq ?? 0;
}

/**
 * Whether a host's `chat_sessions` answer comes from a host that has `chat_update`: the workspace's
 * fields came together, so one session carrying `last_seq` says so. No sessions: not known (null).
 */
export function updatesFrom(raw: unknown[]): boolean | null {
  if (!raw.length) return null;
  return raw.some((s) => !!s && typeof s === "object" && "last_seq" in (s as object));
}

/** A session that is working now (a turn runs, it starts or it stops). */
export function isRunning(m: WsMeta): boolean {
  return m.state === "running" || m.state === "starting" || m.state === "interrupting";
}

/** First line of a message, at most `max` characters (NS-38: 80), or "" for nothing. */
export function firstLine(text: string | null | undefined, max = 80): string {
  const line = (text ?? "").split(/\r?\n/).map((l) => l.trim()).find(Boolean) ?? "";
  return line.length > max ? `${line.slice(0, max - 1).trimEnd()}…` : line;
}

/**
 * What a session is called: its title; else its first message (the host's `first_text`, or the one
 * this device sent, for a host that does not keep it); else the agent's name ("" only when even that is
 * missing: the view says "Untitled").
 */
export function displayTitle(m: WsMeta, localFirst?: string | null): string {
  return m.title?.trim() || firstLine(m.first_text) || firstLine(localFirst) || m.name.trim();
}

// ---------------------------------------------------------------------------------------------
// Status dots (SB-23 / SB-24)

/** A row's dot: needs you (amber), failed (red), unread (blue), running (gray, blinking). */
export type Dot = "awaiting" | "failed" | "unread" | "running";

/** SB-23: the order in which a group's dots stand for it. */
export const DOT_PRIORITY: Record<Dot, number> = { awaiting: 0, failed: 1, unread: 2, running: 3 };

/** Whether a session has something new since this device last looked (SB-25). */
export function isUnread(m: WsMeta, seen: number | undefined, explicit: boolean, current: boolean): boolean {
  if (current || m.archived) return false;
  return explicit || lastSeq(m) > (seen ?? 0);
}

/**
 * SB-24, with ai-remote's fields: archived → nothing; something waits for you → amber; a turn runs →
 * gray; the last turn failed → red; new since you looked → blue; else nothing.
 */
export function dotOf(m: WsMeta, unread: boolean): Dot | null {
  if (m.archived) return null;
  if (m.pending && m.state !== "ended") return "awaiting";
  if (isRunning(m)) return "running";
  if (m.last_status === "failed") return "failed";
  if (unread) return "unread";
  return null;
}

/** The dot that stands for several sessions (a folded group): the one first in `DOT_PRIORITY`. */
export function aggregateDot(dots: Array<Dot | null>): Dot | null {
  let best: Dot | null = null;
  for (const d of dots) if (d && (best === null || DOT_PRIORITY[d] < DOT_PRIORITY[best])) best = d;
  return best;
}

// ---------------------------------------------------------------------------------------------
// Sorting (SB-10)

export type SortMode = "recency" | "created" | "alpha";

const collator = new Intl.Collator(undefined, { numeric: true });

/** Names compare as people read them (numbers by value), ties by code point (SB-10). */
export function compareNames(a: string, b: string): number {
  return collator.compare(a, b) || (a < b ? -1 : a > b ? 1 : 0);
}

/**
 * Sessions in the order of `mode`: last activity (newest first), started (newest first) or title
 * (A→Z). `reverse` turns it around (SB-10 keeps "is reversed" rather than a direction).
 */
export function sortSessions<T extends { meta: WsMeta }>(items: T[], mode: SortMode, title: (x: T) => string, reverse = false): T[] {
  const by = (a: T, b: T) => {
    if (mode === "alpha") return compareNames(title(a), title(b));
    const k = mode === "created" ? "started_at" : "last_activity";
    return b.meta[k] - a.meta[k] || compareNames(title(a), title(b));
  };
  const out = [...items].sort(by);
  return reverse ? out.reverse() : out;
}

// ---------------------------------------------------------------------------------------------
// Groups: pinned first, then one per folder on a computer (SB-08 / SB-15)

/** A folder in a form that compares like `sameFolder` (Windows paths without case or slash kind). */
export function folderKey(p: string): string {
  const v = trimFolder(p);
  const windows = /^([A-Za-z]:|\\\\)/.test(v) || (v.includes("\\") && !v.includes("/"));
  return windows ? v.replace(/\//g, "\\").toLowerCase() : v;
}

export interface WsGroup<T> {
  /** `host` + newline + the folder's key: stable, for folding. */
  key: string;
  host: string;
  folder: string;
  /** The folder's name, with where it is when two groups share a name, and "· computer" off this computer. */
  label: string;
  local: boolean;
  items: T[];
}

/**
 * Splits sessions into the pinned section and one group per folder and computer. Inside both,
 * sessions follow `sort`. Groups are ordered by label (numbers by value), this computer's before
 * others', then by key (SB-15's default order). A label is the folder's name; when two groups have the
 * same name it gains the folder above (`~` for the host user's home, when `home` knows it), and a
 * group on another computer ends with " · <computer>" (SB-15, `sep` U+00B7 with spaces).
 */
export function groupSessions<T extends WsItem>(
  items: T[],
  opts: { pinned: (x: T) => boolean; title: (x: T) => string; home?: (host: string) => string | null; sort?: SortMode },
): { pinned: T[]; groups: WsGroup<T>[] } {
  const sort = opts.sort ?? "recency";
  const pinned = sortSessions(items.filter(opts.pinned), sort, opts.title);
  const byKey = new Map<string, WsGroup<T>>();
  for (const x of items) {
    if (opts.pinned(x)) continue;
    const key = `${x.host}\n${folderKey(x.meta.cwd)}`;
    const g = byKey.get(key);
    if (g) g.items.push(x);
    else byKey.set(key, { key, host: x.host, folder: trimFolder(x.meta.cwd), label: "", local: x.local, items: [x] });
  }
  const groups = [...byKey.values()];
  const named = new Map<string, number>();
  for (const g of groups) {
    // The folder as its latest session names it.
    const latest = g.items.reduce((a, b) => (b.meta.last_activity > a.meta.last_activity ? b : a));
    g.folder = trimFolder(latest.meta.cwd);
    const n = folderName(g.folder).toLowerCase();
    named.set(n, (named.get(n) ?? 0) + 1);
  }
  for (const g of groups) {
    const name = folderName(g.folder) || g.folder;
    const above = parentFolder(g.folder);
    const twin = (named.get(name.toLowerCase()) ?? 0) > 1 && above;
    const where = twin ? ` · ${tildeFolder(above as string, opts.home?.(g.host) ?? null)}` : "";
    g.label = `${name}${where}${g.local ? "" : ` · ${g.host}`}`;
    g.items = sortSessions(g.items, sort, opts.title);
  }
  groups.sort((a, b) => compareNames(a.label, b.label) || Number(b.local) - Number(a.local) || compareNames(a.key, b.key));
  return { pinned, groups };
}

/** SB-19: rows a group shows before "Show N more" (5 when grouped). */
export const GROUP_ROWS = 5;

/**
 * The rows of a group that show: the first `limit` unless opened, plus any of `keep` (the open
 * session, the focused row) that would be cut; what is left out, and whether any of it needs you.
 */
export function truncateGroup<T extends { key: string }>(items: T[], limit: number, keep: Set<string>, open: boolean, needsYou: (x: T) => boolean) {
  if (open || items.length <= limit) return { shown: items, hidden: [] as T[], hiddenNeedsYou: false };
  const shown = items.filter((x, i) => i < limit || keep.has(x.key));
  const hidden = items.filter((x, i) => i >= limit && !keep.has(x.key));
  return { shown, hidden, hiddenNeedsYou: hidden.some(needsYou) };
}

// ---------------------------------------------------------------------------------------------
// Search (SB-06)

/**
 * A query's words: lower case, split at anything that is not a letter or a digit; a query of only
 * such characters is one word as typed (SB-06).
 */
export function searchTokens(query: string): string[] {
  const q = query.trim().toLowerCase();
  if (!q) return [];
  const words = q.split(/[^\p{L}\p{N}]+/u).filter(Boolean);
  return words.length ? words : [q];
}

/** The text each search field holds for one session. */
export interface SearchFields {
  title: string;
  folder: string;
  branch: string;
  computer: string;
}

/**
 * Sessions matching every word (AND). First those whose title holds every word, newest first and at
 * most 100 (SB-06's main results); then those where every word is in the title, folder, branch or
 * computer name, appended after them.
 */
export function searchSessions<T extends { meta: WsMeta }>(items: T[], query: string, fields: (x: T) => SearchFields): T[] {
  const words = searchTokens(query);
  const recent = [...items].sort((a, b) => b.meta.last_activity - a.meta.last_activity);
  if (!words.length) return recent;
  const primary: T[] = [];
  const more: T[] = [];
  for (const x of recent) {
    const f = fields(x);
    const title = f.title.toLowerCase();
    if (words.every((w) => title.includes(w))) primary.push(x);
    else {
      const hay = [title, f.folder.toLowerCase(), f.branch.toLowerCase(), f.computer.toLowerCase()];
      if (words.every((w) => hay.some((h) => h.includes(w)))) more.push(x);
    }
  }
  return [...primary.slice(0, 100), ...more];
}

// ---------------------------------------------------------------------------------------------
// The sidebar's width (SB-02 / SB-03)

export const SIDEBAR = { MIN: 200, MAX: 420, DEFAULT: 288 } as const;

/** The widest the sidebar may be in a window this wide: clamp(innerWidth − 330, 200, 420). */
export function sidebarMax(windowWidth: number): number {
  return Math.min(SIDEBAR.MAX, Math.max(SIDEBAR.MIN, Math.floor(windowWidth) - 330));
}

/** A dragged width within the bounds. */
export function clampSidebar(width: number, windowWidth: number): number {
  return Math.min(sidebarMax(windowWidth), Math.max(SIDEBAR.MIN, Math.round(width)));
}

/** A window at most this wide folds the sidebar away (SB-03: max(700, ⌈width⌉ + 329)). */
export function narrowAt(sidebarWidth: number): number {
  return Math.max(700, Math.ceil(sidebarWidth) + 329);
}

// ---------------------------------------------------------------------------------------------
// The new-session page: recent projects (NS-04) and next steps (NS-08 / NS-09)

/** A computer and a folder on it. */
export interface Place {
  host: string;
  cwd: string;
}

/**
 * The places to offer: every folder of a session and every place a session was started from here,
 * the most recently used first (a place's time is the later of the two), at most `max` (NS-04: 3);
 * the selected place is kept in view by taking the last slot.
 */
export function recentPlaces(sessions: WsItem[], started: Array<Place & { at: number }>, selected: Place | null, max = 3): Place[] {
  const at = new Map<string, Place & { at: number }>();
  const add = (p: Place, t: number) => {
    if (!p.cwd.trim()) return;
    const k = `${p.host}\n${folderKey(p.cwd)}`;
    const prev = at.get(k);
    if (!prev || prev.at < t) at.set(k, { host: p.host, cwd: prev?.cwd ?? trimFolder(p.cwd), at: t });
  };
  for (const s of sessions) add({ host: s.host, cwd: s.meta.cwd }, s.meta.last_activity);
  for (const p of started) add(p, p.at);
  const list = [...at.values()].sort((a, b) => b.at - a.at).map(({ host, cwd }) => ({ host, cwd }));
  const top = list.slice(0, max);
  if (selected && selected.cwd && !top.some((p) => p.host === selected.host && sameFolder(p.cwd, selected.cwd))) {
    const known = list.find((p) => p.host === selected.host && sameFolder(p.cwd, selected.cwd)) ?? selected;
    return [...top.slice(0, max - 1), known];
  }
  return top;
}

/**
 * NS-09: up to `seats` places shared by groups of next steps, in priority order. Each group gets one
 * place, then every free place goes to the first group with more steps than places. A group with
 * more steps than places shows `places − 1` of them and a summary of the rest in its last place.
 */
export function seatSteps<T>(groups: T[][], seats = 3): Array<{ shown: T[]; rest: T[] }> {
  const live = groups.filter((g) => g.length > 0).slice(0, seats);
  const n = live.map(() => 1);
  let free = seats - live.length;
  while (free > 0) {
    const i = live.findIndex((g, j) => g.length > n[j]);
    if (i < 0) break;
    n[i] += 1;
    free -= 1;
  }
  return live.map((g, i) => (g.length <= n[i] ? { shown: g, rest: [] } : { shown: g.slice(0, n[i] - 1), rest: g.slice(n[i] - 1) }));
}

// ---------------------------------------------------------------------------------------------
// The store: every computer's sessions, kept fresh while the workspace is open

/** One computer as the workspace sees it. */
export interface WsComputer {
  name: string;
  online: boolean | null;
  local: boolean;
}

/** What is known of one computer's sessions. */
export interface WsHostState {
  /** `cached`: from the last run (not asked yet); `unsupported`: a host without coding agents; `denied`: this device's level is too low. */
  status: "cached" | "loading" | "ok" | "unsupported" | "denied" | "error";
  error: string | null;
  sessions: WsMeta[];
  /** The host has `chat_update` (null: not known yet). Without it pins stay here and titles / archive cannot change. */
  update: boolean | null;
  fetchedAt: number;
}

export interface WsSnapshot {
  computers: WsComputer[];
  hosts: Record<string, WsHostState>;
  /** The last event number seen of each session (`host/id`). */
  seen: Record<string, number>;
  /** Sessions marked unread by hand (they stay so until opened or marked read). */
  unread: string[];
  /** Pins kept here, for hosts without `chat_update`. */
  pins: string[];
  /** The first message this device sent to a session, for hosts that do not keep it. */
  first: Record<string, string>;
  /** The session open in the workspace now. */
  current: string | null;
  rev: number;
}

export interface WsDeps {
  /** A host tool's JSON result; rejects with the host's error text. */
  call: (host: string, tool: string, args?: Record<string, unknown>) => Promise<unknown>;
  storage?: Pick<Storage, "getItem" | "setItem"> | null;
  now?: () => number;
  /** Polling while the workspace is in view / otherwise (ms). */
  activeMs?: number;
  idleMs?: number;
}

/** Why a chat tool failed, as a status. */
export function failureStatus(message: string): "unsupported" | "denied" | "error" {
  if (/unknown tool '?chat_/i.test(message)) return "unsupported";
  if (/requires level|no longer has access|not paired|revoked/i.test(message)) return "denied";
  return "error";
}

const KEYS = {
  seen: "ai-remote.ws.seen",
  unread: "ai-remote.ws.unread",
  pins: "ai-remote.ws.pins",
  first: "ai-remote.ws.first",
  base: "ai-remote.ws.baselined",
  cache: "ai-remote.ws.cache",
} as const;

/** Most sessions cached per computer, and remembered first messages / seen marks overall. */
const CACHE_PER_HOST = 200;
const MAX_MARKS = 2000;

function keepLast<V>(o: Record<string, V>, max: number): Record<string, V> {
  const keys = Object.keys(o);
  if (keys.length <= max) return o;
  return Object.fromEntries(keys.slice(keys.length - max).map((k) => [k, o[k]]));
}

/**
 * The workspace's sessions. `setComputers` says which computers there are and which are online;
 * `setActive` whether the workspace is in view (polls every 4 s then, else every 20 s). Subscribe for
 * changes (`useSyncExternalStore`).
 */
export class WorkspaceStore {
  private snap: WsSnapshot;
  private listeners = new Set<() => void>();
  private baselined: Set<string>;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private polling = false;
  private active = false;
  private inflight = new Map<string, Promise<void>>();
  private readonly deps: Required<Omit<WsDeps, "storage">> & { storage: WsDeps["storage"] };

  constructor(deps: WsDeps) {
    this.deps = { activeMs: 4000, idleMs: 20000, now: () => Date.now(), ...deps, storage: deps.storage ?? null };
    const cache = this.read<Record<string, WsMeta[]>>(KEYS.cache, {});
    const hosts: Record<string, WsHostState> = {};
    for (const [h, list] of Object.entries(cache)) {
      hosts[h] = { status: "cached", error: null, sessions: list.map(normalizeMeta).filter((m): m is WsMeta => !!m), update: null, fetchedAt: 0 };
    }
    this.snap = {
      computers: [],
      hosts,
      seen: this.read(KEYS.seen, {}),
      unread: this.read(KEYS.unread, []),
      pins: this.read(KEYS.pins, []),
      first: this.read(KEYS.first, {}),
      current: null,
      rev: 0,
    };
    this.baselined = new Set(this.read<string[]>(KEYS.base, []));
  }

  subscribe = (l: () => void) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  get = () => this.snap;

  private read<V>(key: string, fallback: V): V {
    try {
      const v = this.deps.storage?.getItem(key);
      return v ? (JSON.parse(v) as V) : fallback;
    } catch {
      return fallback;
    }
  }

  private write(key: string, value: unknown) {
    try {
      this.deps.storage?.setItem(key, JSON.stringify(value));
    } catch {
      // Storage full or unavailable: it is only remembered for this run.
    }
  }

  private set(next: Partial<WsSnapshot>) {
    this.snap = { ...this.snap, ...next, rev: this.snap.rev + 1 };
    if (next.seen) this.write(KEYS.seen, this.snap.seen);
    if (next.unread) this.write(KEYS.unread, this.snap.unread);
    if (next.pins) this.write(KEYS.pins, this.snap.pins);
    if (next.first) this.write(KEYS.first, this.snap.first);
    this.listeners.forEach((l) => l());
  }

  private setHost(host: string, f: (h: WsHostState) => WsHostState) {
    const prev = this.snap.hosts[host] ?? { status: "loading", error: null, sessions: [], update: null, fetchedAt: 0 };
    const next = f(prev);
    this.set({ hosts: { ...this.snap.hosts, [host]: next } });
    if (next.sessions !== prev.sessions && next.status === "ok") {
      const cache: Record<string, WsMeta[]> = {};
      for (const [h, s] of Object.entries(this.snap.hosts)) if (s.sessions.length) cache[h] = s.sessions.slice(0, CACHE_PER_HOST);
      this.write(KEYS.cache, cache);
    }
  }

  /** The computers to ask; computers that went away are forgotten. */
  setComputers(computers: WsComputer[]) {
    const same =
      computers.length === this.snap.computers.length &&
      computers.every((c, i) => c.name === this.snap.computers[i].name && c.online === this.snap.computers[i].online && c.local === this.snap.computers[i].local);
    if (same) return;
    const cameOnline = computers.filter((c) => c.online === true && !this.snap.computers.some((p) => p.name === c.name && p.online === true));
    this.set({ computers });
    for (const c of cameOnline) if (this.polling) void this.refresh(c.name);
  }

  /** Whether the workspace is in view; starts polling the first time. */
  setActive(active: boolean) {
    const was = this.active;
    this.active = active;
    if (!this.polling) {
      this.polling = true;
      void this.tick();
    } else if (active && !was) {
      // Back in view: look now rather than at the end of the slow interval.
      if (this.timer) clearTimeout(this.timer);
      void this.tick();
    }
  }

  /** Stops polling (the store can start again with `setActive`). */
  stop() {
    this.polling = false;
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
  }

  private async tick() {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    await this.refreshAll();
    if (this.polling) this.timer = setTimeout(() => void this.tick(), this.active ? this.deps.activeMs : this.deps.idleMs);
  }

  /** Asks every online computer that can have sessions. */
  async refreshAll() {
    const ask = this.snap.computers.filter((c) => c.online === true && this.snap.hosts[c.name]?.status !== "unsupported");
    await Promise.all(ask.map((c) => this.refresh(c.name)));
  }

  /** Asks one computer for its sessions (one request at a time per computer). */
  refresh(host: string): Promise<void> {
    const running = this.inflight.get(host);
    if (running) return running;
    const p = this.fetch(host).finally(() => this.inflight.delete(host));
    this.inflight.set(host, p);
    return p;
  }

  private async fetch(host: string) {
    if (!this.snap.hosts[host] || this.snap.hosts[host].status === "cached") this.setHost(host, (h) => ({ ...h, status: h.sessions.length ? h.status : "loading" }));
    try {
      const v = (await this.deps.call(host, "chat_sessions", {})) as { sessions?: unknown[] };
      const raw = Array.isArray(v?.sessions) ? v.sessions : [];
      const sessions = raw.map(normalizeMeta).filter((m): m is WsMeta => !!m && !m.archived);
      const learned = updatesFrom(raw);
      this.setHost(host, (h) => ({
        status: "ok",
        error: null,
        sessions,
        // A host once seen with the fields (or answering chat_update) keeps them; one that refused stays refused.
        update: h.update === false && learned !== true ? false : learned ?? h.update,
        fetchedAt: this.deps.now(),
      }));
      this.markSeen(host, sessions);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      this.setHost(host, (h) => ({ ...h, status: failureStatus(msg), error: msg, fetchedAt: this.deps.now() }));
    }
  }

  /**
   * The first time a computer answers, everything on it counts as seen (only what happens from then
   * on is new); the open session is always seen up to its last event.
   */
  private markSeen(host: string, sessions: WsMeta[]) {
    const seen = { ...this.snap.seen };
    let changed = false;
    const first = !this.baselined.has(host);
    for (const m of sessions) {
      const k = sessionKey(host, m.id);
      const s = lastSeq(m);
      if ((first && seen[k] === undefined) || (k === this.snap.current && (seen[k] ?? -1) < s)) {
        seen[k] = s;
        changed = true;
      }
    }
    if (first) {
      this.baselined.add(host);
      this.write(KEYS.base, [...this.baselined]);
    }
    if (changed) this.set({ seen: keepLast(seen, MAX_MARKS) });
  }

  /** Every session, newest activity first, with its computer. */
  items(): WsItem[] {
    const out: WsItem[] = [];
    for (const [host, h] of Object.entries(this.snap.hosts)) {
      const c = this.snap.computers.find((x) => x.name === host);
      // Computers that are no longer paired are not shown.
      if (!c && this.snap.computers.length) continue;
      for (const meta of h.sessions) out.push({ host, key: sessionKey(host, meta.id), meta, local: !!c?.local, online: c?.online ?? null });
    }
    return out.sort((a, b) => b.meta.last_activity - a.meta.last_activity);
  }

  find(host: string, id: string): WsItem | null {
    return this.items().find((x) => x.host === host && x.meta.id === id) ?? null;
  }

  isPinned(x: WsItem): boolean {
    return !!x.meta.pinned || this.snap.pins.includes(x.key);
  }

  unread(x: WsItem): boolean {
    return isUnread(x.meta, this.snap.seen[x.key], this.snap.unread.includes(x.key), x.key === this.snap.current);
  }

  dot(x: WsItem): Dot | null {
    return dotOf(x.meta, this.unread(x));
  }

  title(x: WsItem): string {
    return displayTitle(x.meta, this.snap.first[x.key]);
  }

  /** The session open in the workspace (null: none); opening one marks it read. */
  setCurrent(key: string | null) {
    if (key === this.snap.current) return;
    this.set({ current: key });
    if (key) this.markRead(key);
  }

  markRead(key: string) {
    const x = this.items().find((i) => i.key === key);
    const seen = x ? { ...this.snap.seen, [key]: lastSeq(x.meta) } : this.snap.seen;
    this.set({ seen: keepLast(seen, MAX_MARKS), unread: this.snap.unread.filter((k) => k !== key) });
  }

  markUnread(key: string) {
    if (!this.snap.unread.includes(key)) this.set({ unread: [...this.snap.unread, key] });
  }

  /** Remembers the first message sent from here (a title for hosts that do not keep one). */
  rememberFirst(host: string, id: string, text: string) {
    const first = keepLast({ ...this.snap.first, [sessionKey(host, id)]: text.slice(0, 200) }, 500);
    this.set({ first });
  }

  /** Shows a session right away (just started here), before the host lists it. */
  addSession(host: string, meta: WsMeta) {
    this.setHost(host, (h) => ({ ...h, status: h.status === "cached" || h.status === "loading" ? "ok" : h.status, sessions: [meta, ...h.sessions.filter((m) => m.id !== meta.id)] }));
    const k = sessionKey(host, meta.id);
    this.set({ seen: { ...this.snap.seen, [k]: lastSeq(meta) } });
  }

  /** Changes what is known of a session here (an event seen on its open conversation). */
  patchSession(host: string, id: string, patch: Partial<WsMeta>) {
    const h = this.snap.hosts[host];
    if (!h || !h.sessions.some((m) => m.id === id)) return;
    this.setHost(host, (s) => ({ ...s, sessions: s.sessions.map((m) => (m.id === id ? { ...m, ...patch } : m)).filter((m) => !m.archived) }));
  }

  /**
   * Pins, archives or renames a session through `chat_update` (`title: ""` goes back to the automatic
   * title). Shown at once and put back when the host refuses. A host without `chat_update`: a pin is
   * kept on this device; renaming and archiving reject with `unsupported`.
   */
  async update(host: string, id: string, patch: { title?: string; pinned?: boolean; archived?: boolean }, known?: WsMeta): Promise<void> {
    const key = sessionKey(host, id);
    const h = this.snap.hosts[host];
    // `known`: a session no longer listed (archived), so that undoing brings it back at once.
    const listed = h?.sessions.find((m) => m.id === id) ?? null;
    const before = listed ?? known ?? null;
    if (!listed && known && patch.archived === false) this.addSession(host, { ...known, archived: false });
    const localPin = () => {
      if (patch.pinned === undefined) return;
      this.set({ pins: patch.pinned ? [...this.snap.pins.filter((k) => k !== key), key] : this.snap.pins.filter((k) => k !== key) });
    };
    if (h?.update === false) {
      if (patch.title !== undefined || patch.archived !== undefined) throw new Error("unsupported");
      localPin();
      return;
    }
    // A pin kept here is dropped as soon as the host keeps it.
    if (patch.pinned === false && this.snap.pins.includes(key)) {
      this.set({ pins: this.snap.pins.filter((k) => k !== key) });
      if (!before?.pinned) return;
    }
    const optimistic: Partial<WsMeta> = { ...patch };
    if (patch.title !== undefined) {
      optimistic.title = patch.title.trim() || null;
      optimistic.title_source = patch.title.trim() ? "user" : "auto";
    }
    if (before) this.patchSession(host, id, optimistic);
    try {
      const v = await this.deps.call(host, "chat_update", { session: id, ...patch });
      const meta = normalizeMeta(v && typeof v === "object" && "meta" in (v as object) ? (v as { meta: unknown }).meta : v);
      const fresh = meta && meta.id === id ? meta : null;
      this.setHost(host, (s) => ({
        ...s,
        update: true,
        sessions: (s.sessions.some((m) => m.id === id)
          ? s.sessions.map((m) => (m.id === id ? { ...m, ...(fresh ?? optimistic) } : m))
          : fresh
            ? [fresh, ...s.sessions]
            : s.sessions
        ).filter((m) => !m.archived),
      }));
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      // Put back what was shown (an archived session that failed to come back stays away).
      if (before && !(patch.archived === false && !listed)) {
        this.setHost(host, (s) => ({ ...s, sessions: s.sessions.some((m) => m.id === id) ? s.sessions.map((m) => (m.id === id ? before : m)) : [before, ...s.sessions] }));
      } else if (patch.archived === false && !listed) {
        this.setHost(host, (s) => ({ ...s, sessions: s.sessions.filter((m) => m.id !== id) }));
      }
      if (failureStatus(msg) === "unsupported") {
        this.setHost(host, (s) => ({ ...s, update: false }));
        if (patch.title !== undefined || patch.archived !== undefined) throw new Error("unsupported");
        localPin();
        return;
      }
      throw e instanceof Error ? e : new Error(msg);
    }
  }

  /** Ends a session on its host (its transcript stays). */
  async end(host: string, id: string) {
    await this.deps.call(host, "chat_end", { session: id });
    this.patchSession(host, id, { state: "ended" });
  }
}
