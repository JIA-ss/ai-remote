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

/**
 * What one chat event (design 1.2) changes in its session's metadata, for a session open here: its
 * title, state, what waits for a person, how the turn ended, its last event. Null: nothing.
 */
export function eventPatch(e: { t: string; seq?: number; ts?: number; [k: string]: unknown }): Partial<WsMeta> | null {
  const p: Partial<WsMeta> = {};
  if (typeof e.seq === "number" && e.seq > 0) {
    p.last_seq = e.seq;
    if (typeof e.ts === "number") p.last_activity = e.ts;
  }
  switch (e.t) {
    case "title":
      if (typeof e.title === "string" && e.title.trim()) {
        p.title = e.title.trim();
        p.title_source = e.source === "user" || e.source === "auto" ? e.source : "agent";
      }
      break;
    case "state":
      if (typeof e.state === "string") p.state = e.state as ChatState;
      break;
    case "permission":
      p.pending = "permission";
      break;
    case "question":
      p.pending = "question";
      break;
    case "plan_review":
      p.pending = "plan";
      break;
    case "resolved":
      p.pending = null;
      break;
    case "turn_end":
      p.pending = null;
      if (e.status === "completed" || e.status === "failed" || e.status === "interrupted") p.last_status = e.status;
      p.last_reason = typeof e.reason === "string" ? e.reason.slice(0, 300) : null;
      break;
    case "mode":
      if (typeof e.mode === "string") p.permission_mode = e.mode;
      break;
  }
  return Object.keys(p).length ? p : null;
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
export function dotOf(m: WsMeta, unread: boolean, acked = false): Dot | null {
  if (m.archived) return null;
  if (m.pending && m.state !== "ended") return "awaiting";
  if (isRunning(m)) return "running";
  if (m.last_status === "failed" && !acked) return "failed";
  if (unread) return "unread";
  return null;
}

/**
 * SB-26: "Mark as done" keeps the turn count it was given at; a failure is put aside while no turn
 * was sent since (a later turn that fails shows again). Not the event number: renaming, pinning or a
 * mode change add events without a new turn and must not bring the failure back.
 */
export function isAcked(m: WsMeta, ackedTurns: number | undefined): boolean {
  return ackedTurns !== undefined && ackedTurns >= m.turns;
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
  opts: { pinned: (x: T) => boolean; title: (x: T) => string; home?: (host: string) => string | null; sort?: SortMode; reversed?: boolean },
): { pinned: T[]; groups: WsGroup<T>[] } {
  const sort = opts.sort ?? "recency";
  const pinned = sortSessions(items.filter(opts.pinned), sort, opts.title, opts.reversed);
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
    g.items = sortSessions(g.items, sort, opts.title, opts.reversed);
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
// How the list groups, sorts and filters (SB-09..SB-14, SB-46)

/** SB-09: by folder on a computer (the default here), by the groups made here (SB-08), by day, by state, or not at all. */
export type GroupBy = "project" | "custom" | "date" | "state" | "none";
/** SB-11: the sessions listed — those not archived (default), the archived ones, or both. */
export type StatusFilter = "active" | "archived" | "all";

export interface ListPrefs {
  groupBy: GroupBy;
  sort: SortMode;
  /** SB-10 keeps "turned around" rather than a direction: names A→Z and the newest first are not. */
  reversed: boolean;
  status: StatusFilter;
}

export const DEFAULT_LIST: ListPrefs = { groupBy: "project", sort: "recency", reversed: false, status: "active" };

const pick = <V extends string>(v: unknown, ok: readonly V[], fallback: V): V => (ok.includes(v as V) ? (v as V) : fallback);

/** Saved settings as they can be used (anything unknown takes its default). */
export function normalizeList(raw: unknown): ListPrefs {
  const o = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  return {
    groupBy: pick(o.groupBy, ["project", "custom", "date", "state", "none"] as const, DEFAULT_LIST.groupBy),
    sort: pick(o.sort, ["recency", "created", "alpha"] as const, DEFAULT_LIST.sort),
    reversed: o.reversed === true,
    status: pick(o.status, ["active", "archived", "all"] as const, DEFAULT_LIST.status),
  };
}

export function isDefaultList(p: ListPrefs): boolean {
  return p.groupBy === DEFAULT_LIST.groupBy && p.sort === DEFAULT_LIST.sort && p.reversed === DEFAULT_LIST.reversed && p.status === DEFAULT_LIST.status;
}

/** SB-11: the list is filtered while it shows anything but the sessions not archived. */
export function isFiltered(p: ListPrefs): boolean {
  return p.status !== "active";
}

/** Whether the list runs oldest first (A→Z for names): names go up unless turned around, times go down. */
export function isAscending(p: ListPrefs): boolean {
  return p.sort === "alpha" ? !p.reversed : p.reversed;
}

/** The settings with the order going up (`true`) or down, kept as SB-10's "turned around". */
export function withAscending(p: ListPrefs, ascending: boolean): ListPrefs {
  return { ...p, reversed: p.sort === "alpha" ? !ascending : ascending };
}

/** Whether a session belongs in the list under a status filter. */
export function shownByStatus(m: WsMeta, status: StatusFilter): boolean {
  return status === "all" || (status === "archived") === !!m.archived;
}

/** SB-19: rows shown before "Show N more" — five in a group, twenty when the list is not grouped. */
export function rowsBeforeMore(groupBy: GroupBy): number {
  return groupBy === "none" ? 20 : GROUP_ROWS;
}

/** SB-13's buckets: today, yesterday, the five days before (each its own), and older. */
export type DayKind = "today" | "yesterday" | "day" | "older";
export interface DayBucket<T> {
  key: string;
  kind: DayKind;
  /** The bucket's day (local midnight, ms); for "older", the start of the first day not in a bucket. */
  day: number;
  items: T[];
}

/**
 * Sessions by the day they were last active (or started, when sorting by start): today, yesterday,
 * one bucket for each of the five days before, then older (SB-13). Empty buckets are left out; items
 * keep their order inside a bucket. `oldestFirst` turns the buckets around (the order going up, not
 * by name). Days are the local calendar's.
 */
export function dateBuckets<T extends { meta: WsMeta }>(items: T[], now: number, by: "last_activity" | "started_at", oldestFirst = false): DayBucket<T>[] {
  const d = new Date(now);
  const midnight = (back: number) => new Date(d.getFullYear(), d.getMonth(), d.getDate() - back).getTime();
  const starts = Array.from({ length: 7 }, (_, i) => midnight(i));
  const buckets: DayBucket<T>[] = starts.map((day, i) => ({ key: i === 0 ? "today" : i === 1 ? "yesterday" : `day${i}`, kind: i === 0 ? "today" : i === 1 ? "yesterday" : "day", day, items: [] }));
  const older: DayBucket<T> = { key: "older", kind: "older", day: starts[6], items: [] };
  for (const x of items) {
    const at = x.meta[by];
    const i = starts.findIndex((s) => at >= s);
    (i < 0 ? older : buckets[i]).items.push(x);
  }
  const out = [...buckets, older].filter((b) => b.items.length);
  return oldestFirst ? out.reverse() : out;
}

/** SB-14's buckets, in this order: needs you (failed too), working, to look at, done, idle. */
export type StateBucket = "blocked" | "working" | "ready" | "done" | "idle";
export const STATE_ORDER: readonly StateBucket[] = ["blocked", "working", "ready", "done", "idle"];

/** Which state bucket a session is in, from its dot: archived and ended sessions are done. */
export function stateBucket(m: WsMeta, dot: Dot | null): StateBucket {
  if (dot === "awaiting" || dot === "failed") return "blocked";
  if (dot === "running") return "working";
  if (dot === "unread") return "ready";
  if (m.archived || m.state === "ended") return "done";
  return "idle";
}

/** Sessions by state bucket, in SB-14's order, without empty buckets; items keep their order. */
export function stateBuckets<T>(items: T[], bucketOf: (x: T) => StateBucket): Array<{ key: StateBucket; items: T[] }> {
  return STATE_ORDER.map((key) => ({ key, items: items.filter((x) => bucketOf(x) === key) })).filter((b) => b.items.length);
}

// ---------------------------------------------------------------------------------------------
// Groups made here, the pinned order (SB-08 / SB-15 / SB-36 / SB-42), the "Older" bucket's bulk
// action (SB-38), and how rows are drawn: density (SB-04) and the metadata line (SB-28). All of it
// stays on this device: a group holds sessions by `host/id`, so sessions of several computers can
// share one, and a host only ever sees its session ids.

/** A group made here: a name (1–200 characters, SB-08) and a stable id. */
export interface CustomGroup {
  id: string;
  name: string;
}

export interface GroupsState {
  /** The groups in the order they are drawn (SB-15: the person's order). */
  groups: CustomGroup[];
  /** Session (`host/id`) → the group it was moved into; a session is in one group at most. */
  members: Record<string, string>;
  /** Pinned sessions in the order they were put (SB-42); pins not in it follow, in the list's order. */
  pinOrder: string[];
}

export const EMPTY_GROUPS: GroupsState = { groups: [], members: {}, pinOrder: [] };
/** SB-08: a group's name is 1–200 characters; at most 100 sessions move in one go. */
export const GROUP_NAME_MAX = 200;
export const MOVE_MAX = 100;

/** A name as kept: inner runs of space made one, trimmed, at most 200 characters; null for an empty one. */
export function cleanGroupName(raw: string): string | null {
  const v = raw.replace(/\s+/g, " ").trim();
  return v ? v.slice(0, GROUP_NAME_MAX) : null;
}

/** Saved groups as they can be used: broken entries dropped, members of groups that are gone forgotten. */
export function normalizeGroups(raw: unknown): GroupsState {
  const o = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  const seen = new Set<string>();
  const groups: CustomGroup[] = [];
  for (const g of Array.isArray(o.groups) ? o.groups : []) {
    const r = g && typeof g === "object" ? (g as Record<string, unknown>) : {};
    const id = typeof r.id === "string" ? r.id : "";
    const name = typeof r.name === "string" ? cleanGroupName(r.name) : null;
    if (!id || !name || seen.has(id)) continue;
    seen.add(id);
    groups.push({ id, name });
  }
  const members: Record<string, string> = {};
  if (o.members && typeof o.members === "object") {
    for (const [k, v] of Object.entries(o.members as Record<string, unknown>)) if (typeof v === "string" && seen.has(v)) members[k] = v;
  }
  const pinOrder = Array.isArray(o.pinOrder) ? [...new Set(o.pinOrder.filter((k): k is string => typeof k === "string"))] : [];
  return { groups, members, pinOrder };
}

/** A new group at the end of the order; null when the name is empty or the id is taken. */
export function createGroup(s: GroupsState, name: string, id: string): { state: GroupsState; group: CustomGroup } | null {
  const clean = cleanGroupName(name);
  if (!clean || !id || s.groups.some((g) => g.id === id)) return null;
  const group = { id, name: clean };
  return { state: { ...s, groups: [...s.groups, group] }, group };
}

/** A group renamed; the same state when the name is empty or unchanged. */
export function renameGroup(s: GroupsState, id: string, name: string): GroupsState {
  const clean = cleanGroupName(name);
  if (!clean || !s.groups.some((g) => g.id === id && g.name !== clean)) return s;
  return { ...s, groups: s.groups.map((g) => (g.id === id ? { ...g, name: clean } : g)) };
}

export interface DeletedGroup {
  group: CustomGroup;
  index: number;
  members: string[];
}

/**
 * SB-08: a group deleted, its sessions back in the list's own section. What comes back lets the toast's
 * Undo put the group, its place and its sessions back as they were.
 */
export function deleteGroup(s: GroupsState, id: string): { state: GroupsState; undo: DeletedGroup | null } {
  const index = s.groups.findIndex((g) => g.id === id);
  if (index < 0) return { state: s, undo: null };
  const members = Object.keys(s.members).filter((k) => s.members[k] === id);
  const rest = { ...s.members };
  for (const k of members) delete rest[k];
  return { state: { ...s, groups: s.groups.filter((g) => g.id !== id), members: rest }, undo: { group: s.groups[index], index, members } };
}

/** Undoes `deleteGroup`: the group at its place again, with its sessions that went into no other group since. */
export function restoreGroup(s: GroupsState, undo: DeletedGroup): GroupsState {
  if (s.groups.some((g) => g.id === undo.group.id)) return s;
  const groups = [...s.groups];
  groups.splice(Math.min(undo.index, groups.length), 0, undo.group);
  const members = { ...s.members };
  for (const k of undo.members) if (!members[k]) members[k] = undo.group.id;
  return { ...s, groups, members };
}

export interface GroupMove {
  key: string;
  from: string | null;
}

/**
 * Sessions moved into a group (`null`: out of any group). At most `MOVE_MAX` in one go (SB-08); an
 * unknown group moves nothing. `moved` lists the sessions whose group changed, with the group each was in.
 */
export function moveToGroup(s: GroupsState, keys: string[], id: string | null): { state: GroupsState; moved: GroupMove[] } {
  if (id !== null && !s.groups.some((g) => g.id === id)) return { state: s, moved: [] };
  const members = { ...s.members };
  const moved: GroupMove[] = [];
  for (const key of [...new Set(keys)].slice(0, MOVE_MAX)) {
    const from = members[key] ?? null;
    if (from === id) continue;
    moved.push({ key, from });
    if (id === null) delete members[key];
    else members[key] = id;
  }
  return moved.length ? { state: { ...s, members }, moved } : { state: s, moved };
}

/** Puts the sessions of a move back into the groups they came from (the toast's Undo). */
export function undoMove(s: GroupsState, moved: GroupMove[]): GroupsState {
  const members = { ...s.members };
  for (const { key, from } of moved) {
    if (from === null || !s.groups.some((g) => g.id === from)) delete members[key];
    else members[key] = from;
  }
  return { ...s, members };
}

/**
 * A list with `key` placed before the item that was at `before` (0..length; the end for anything past
 * it), as a drag's insertion line or one step of ⌥↑ / ⌥↓ says. A key not in the list is inserted.
 */
export function moveKey(keys: string[], key: string, before: number): string[] {
  return placeKeys(keys, [key], before);
}

/** Several keys placed together, in their given order, before the item that was at `before` (a dragged selection). */
export function placeKeys(keys: string[], moving: string[], before: number): string[] {
  const take = new Set(moving);
  const at = Math.max(0, Math.min(Math.round(before), keys.length));
  const rest = keys.filter((k) => !take.has(k));
  const shift = keys.slice(0, at).filter((k) => take.has(k)).length;
  rest.splice(Math.min(at - shift, rest.length), 0, ...[...take]);
  return rest;
}

/** SB-15: a group moved before the group that was at `before` (drag or ⌥↑ / ⌥↓). */
export function moveGroup(s: GroupsState, id: string, before: number): GroupsState {
  const ids = s.groups.map((g) => g.id);
  if (!ids.includes(id)) return s;
  const order = moveKey(ids, id, before);
  if (order.every((x, i) => x === ids[i])) return s;
  const byId = new Map(s.groups.map((g) => [g.id, g]));
  return { ...s, groups: order.map((x) => byId.get(x) as CustomGroup) };
}

/** Items in a remembered order first (SB-15 / SB-42), then the others in the order they came. */
export function orderBy<T>(items: T[], order: readonly string[], keyOf: (x: T) => string): T[] {
  if (!order.length) return items;
  const at = new Map(order.map((k, i) => [k, i]));
  const known = items.filter((x) => at.has(keyOf(x))).sort((a, b) => (at.get(keyOf(a)) as number) - (at.get(keyOf(b)) as number));
  return [...known, ...items.filter((x) => !at.has(keyOf(x)))];
}

/**
 * "Custom groups" (SB-08's section layout): one section per group made here, in its order (empty ones
 * too, so a row can be dropped there), then the sessions in no group. Items keep their order.
 */
export function customSections<T extends { key: string }>(items: T[], s: GroupsState): { sections: Array<{ group: CustomGroup; items: T[] }>; rest: T[] } {
  const by = new Map(s.groups.map((g) => [g.id, [] as T[]]));
  const rest: T[] = [];
  for (const x of items) {
    const list = by.get(s.members[x.key] ?? "");
    if (list) list.push(x);
    else rest.push(x);
  }
  return { sections: s.groups.map((group) => ({ group, items: by.get(group.id) as T[] })), rest };
}

/** SB-38: sessions in the "Older" bucket a week or more without activity, not archived yet. */
export const OLDER_DAYS = 7;
export function olderSessions<T extends { meta: WsMeta }>(items: T[], now: number): T[] {
  const before = now - OLDER_DAYS * 86_400_000;
  return items.filter((x) => !x.meta.archived && x.meta.last_activity < before);
}

/** SB-04: the two densities. Compact is the default (the session list's 28 px rows); comfortable is 30 px (desktop). */
export type Density = "compact" | "comfortable";
export const DENSITY: Record<Density, { row: number; font: number; groupFont: number; radius: number; groupTop: number; gap: number; meta: number }> = {
  compact: { row: 28, font: 13, groupFont: 12, radius: 6, groupTop: 12, gap: 2, meta: 20 },
  comfortable: { row: 30, font: 14, groupFont: 13, radius: 8, groupTop: 14, gap: 2, meta: 20 },
};

/** How rows are drawn: the density and whether they carry the metadata line (SB-12's "Show"). */
export interface DisplayPrefs {
  density: Density;
  meta: boolean;
}
export const DEFAULT_DISPLAY: DisplayPrefs = { density: "compact", meta: false };

export function normalizeDisplay(raw: unknown): DisplayPrefs {
  const o = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  return { density: pick(o.density, ["compact", "comfortable"] as const, DEFAULT_DISPLAY.density), meta: o.meta === true };
}

export function isDefaultDisplay(d: DisplayPrefs): boolean {
  return d.density === DEFAULT_DISPLAY.density && d.meta === DEFAULT_DISPLAY.meta;
}

/**
 * SB-28's second line: blocked (something waits for a person, unless the row sits under "Needs you"),
 * else the branch, then the folder (left out when the group header already names it), then the computer
 * when it is not this one and the header does not say it. Null fields are left out; null: no line at all.
 */
export function metaLine(
  m: WsMeta,
  o: { groupFolder?: string | null; host?: string | null; underBlocked?: boolean },
): { blocked: WsPending | null; branch: string | null; folder: string | null; host: string | null } | null {
  if (!o.underBlocked && m.pending && m.state !== "ended") return { blocked: m.pending, branch: null, folder: null, host: null };
  const branch = m.branch?.trim() || null;
  const name = folderName(m.cwd) || trimFolder(m.cwd) || null;
  const folder = name && !(o.groupFolder && sameFolder(o.groupFolder, m.cwd)) ? name : null;
  const host = o.host?.trim() || null;
  if (!branch && !folder && !host) return null;
  return { blocked: null, branch, folder, host };
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

/** SB-41: the same Ctrl+Shift chord on every platform; Cmd and Alt never join it. */
export function sessionJumpChord(e: { ctrlKey: boolean; shiftKey: boolean; metaKey: boolean; altKey: boolean }): boolean {
  return e.ctrlKey && e.shiftKey && !e.metaKey && !e.altKey;
}

/** Shift changes a number row's `key` to punctuation, so physical digit keys take precedence. */
export function sessionJumpNumber(e: { key: string; code: string }): number | null {
  const digit = /^(?:Digit|Numpad)([1-9])$/.exec(e.code)?.[1] ?? (/^[1-9]$/.test(e.key) ? e.key : null);
  return digit === null ? null : Number(digit);
}

export const SESSION_JUMP = { HOLD_MS: 200, COUNT: 9 } as const;

/** NS-34: collapse when full labels no longer fit; restore only with more than 2 px spare. */
export function compactEnvironment(compact: boolean, available: number, required: number): boolean {
  if (!Number.isFinite(available) || !Number.isFinite(required) || available <= 0 || required <= 0) return compact;
  return compact ? available <= required + 2 : available < required;
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
export function recentPlaces(sessions: WsItem[], started: Array<Place & { at: number }>, selected: Place | null, max = 3): Array<Place & { at: number }> {
  const at = new Map<string, Place & { at: number }>();
  const add = (p: Place, t: number) => {
    if (!p.cwd.trim()) return;
    const k = `${p.host}\n${folderKey(p.cwd)}`;
    const prev = at.get(k);
    if (!prev || prev.at < t) at.set(k, { host: p.host, cwd: prev?.cwd ?? trimFolder(p.cwd), at: t });
  };
  for (const s of sessions) add({ host: s.host, cwd: s.meta.cwd }, s.meta.last_activity);
  for (const p of started) add(p, p.at);
  const list = [...at.values()].sort((a, b) => b.at - a.at);
  const top = list.slice(0, max);
  if (selected && selected.cwd && !top.some((p) => p.host === selected.host && sameFolder(p.cwd, selected.cwd))) {
    const known = list.find((p) => p.host === selected.host && sameFolder(p.cwd, selected.cwd)) ?? { ...selected, at: 0 };
    return [...top.slice(0, max - 1), known];
  }
  return top;
}

/**
 * NS-04: while the page stays open, rows already shown keep their order; a new one goes before the
 * first shown row older than it (rows no longer offered leave).
 */
export function keepOrder<T extends Place & { at: number }>(shown: T[], next: T[]): T[] {
  const same = (a: Place, b: Place) => a.host === b.host && sameFolder(a.cwd, b.cwd);
  const out = shown.map((s) => next.find((n) => same(n, s))).filter((x): x is T => !!x);
  for (const n of next) {
    if (out.some((o) => same(o, n))) continue;
    const i = out.findIndex((o) => o.at < n.at);
    if (i < 0) out.push(n);
    else out.splice(i, 0, n);
  }
  return out;
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
  /** "Marked as done" (SB-26): the event number each session was put aside at. */
  acked: Record<string, number>;
  /** Archived sessions are asked for and kept (the list shows them). */
  withArchived: boolean;
  /** The first message this device sent to a session, for hosts that do not keep it. */
  first: Record<string, string>;
  /** The session open in the workspace now. */
  current: string | null;
  /** Groups made here, who is in them, and the pinned order (all on this device). */
  groups: GroupsState;
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
  acked: "ai-remote.ws.ackedTurns",
  cache: "ai-remote.ws.cache",
  groups: "ai-remote.ws.groups",
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
  private mutations = new Map<string, Promise<unknown>>();
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
      acked: this.read(KEYS.acked, {}),
      withArchived: false,
      first: this.read(KEYS.first, {}),
      current: null,
      groups: normalizeGroups(this.read<unknown>(KEYS.groups, null)),
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
    if (next.acked) this.write(KEYS.acked, this.snap.acked);
    if (next.groups) this.write(KEYS.groups, this.snap.groups);
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
      const withArchived = this.snap.withArchived;
      // `archived: true` lists only the archived sessions, so "with archived" asks twice and merges.
      const list = async (args: Record<string, unknown>) => {
        const v = (await this.deps.call(host, "chat_sessions", args)) as { sessions?: unknown[] };
        return Array.isArray(v?.sessions) ? v.sessions : [];
      };
      const [live, archived] = await Promise.all([list({}), withArchived ? list({ archived: true }) : Promise.resolve([])]);
      const ids = new Set(live.map((r) => (r && typeof r === "object" ? (r as { id?: unknown }).id : undefined)));
      const raw = [...live, ...archived.filter((r) => !(r && typeof r === "object" && ids.has((r as { id?: unknown }).id)))];
      // An answer to a question asked before the list changed its filter keeps what that filter wants.
      const sessions = raw.map(normalizeMeta).filter((m): m is WsMeta => !!m && (this.snap.withArchived || !m.archived));
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
      for (const meta of h.sessions) if (this.keeps(meta)) out.push({ host, key: sessionKey(host, meta.id), meta, local: !!c?.local, online: c?.online ?? null });
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
    return dotOf(x.meta, this.unread(x), this.acked(x));
  }

  /** Put aside with "Mark as done" since its last event (SB-26). */
  acked(x: WsItem): boolean {
    return isAcked(x.meta, this.snap.acked[x.key]);
  }

  /**
   * "Mark as done" (SB-26): a failure stops showing and the session counts as read. Returns what puts
   * it back as it was (the undo of the toast).
   */
  markDone(key: string): () => void {
    const x = this.items().find((i) => i.key === key);
    if (!x) return () => {};
    const was = { acked: this.snap.acked[key], unread: this.unread(x), explicit: this.snap.unread.includes(key), seen: this.snap.seen[key] };
    this.set({ acked: keepLast({ ...this.snap.acked, [key]: x.meta.turns }, MAX_MARKS) });
    this.markRead(key);
    return () => {
      const acked = { ...this.snap.acked };
      if (was.acked === undefined) delete acked[key];
      else acked[key] = was.acked;
      const seen = { ...this.snap.seen };
      if (was.seen === undefined) delete seen[key];
      else seen[key] = was.seen;
      this.set({ acked, seen, unread: was.explicit ? [...this.snap.unread.filter((k) => k !== key), key] : this.snap.unread });
    };
  }

  /**
   * The groups made here after a change (lib functions compute it from `get().groups`); members are
   * kept for the last 2000 sessions, and so is the pinned order.
   */
  setGroups(next: GroupsState) {
    if (next === this.snap.groups) return;
    this.set({ groups: { ...next, members: keepLast(next.members, MAX_MARKS), pinOrder: next.pinOrder.slice(-MAX_MARKS) } });
  }

  /** The group a session was moved into here, if it still exists. */
  groupOf(key: string): CustomGroup | null {
    const id = this.snap.groups.members[key];
    return (id && this.snap.groups.groups.find((g) => g.id === id)) || null;
  }

  /** Whether archived sessions are asked for and kept (the list's filter shows them); asks again when it changes. */
  setArchived(on: boolean) {
    if (on === this.snap.withArchived) return;
    const hosts: Record<string, WsHostState> = {};
    for (const [h, s] of Object.entries(this.snap.hosts)) hosts[h] = on ? s : { ...s, sessions: s.sessions.filter((m) => !m.archived) };
    this.set({ withArchived: on, hosts });
    if (on && this.polling) void this.refreshAll();
  }

  private keeps(m: WsMeta): boolean {
    return this.snap.withArchived || !m.archived;
  }

  title(x: WsItem): string {
    return displayTitle(x.meta, this.snap.first[x.key]);
  }

  /** The session open in the workspace (null: none); opening one marks it read, and so does leaving it. */
  setCurrent(key: string | null) {
    const was = this.snap.current;
    if (key === was) return;
    if (was) this.markRead(was);
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

  readMark(key: string): { seen: number | undefined; explicit: boolean } {
    return { seen: this.snap.seen[key], explicit: this.snap.unread.includes(key) };
  }

  /** SB-34: one snapshot and one write per existing storage key, rather than a write per row. */
  changeRead(keys: string[], read: boolean) {
    const seen = { ...this.snap.seen };
    const unread = new Set(this.snap.unread);
    const items = new Map(this.items().map((item) => [item.key, item]));
    const changes: Array<{ key: string; before: ReturnType<WorkspaceStore["readMark"]>; after: ReturnType<WorkspaceStore["readMark"]> }> = [];
    for (const key of new Set(keys)) {
      const x = items.get(key);
      if (!x || isRunning(x.meta) || x.meta.pending) continue;
      const before = this.readMark(key);
      if (read) { seen[key] = lastSeq(x.meta); unread.delete(key); }
      else unread.add(key);
      changes.push({ key, before, after: { seen: seen[key], explicit: unread.has(key) } });
    }
    if (changes.length) this.set({ seen: keepLast(seen, MAX_MARKS), unread: [...unread] });
    return changes;
  }

  /** Undo only unchanged marks: opening a session or receiving newer events must win. */
  restoreRead(changes: ReturnType<WorkspaceStore["changeRead"]>): string[] {
    const seen = { ...this.snap.seen };
    const unread = new Set(this.snap.unread);
    const applied: string[] = [];
    const items = new Map(this.items().map((item) => [item.key, item]));
    for (const change of changes) {
      const current = this.readMark(change.key);
      const x = items.get(change.key);
      if (!x || isRunning(x.meta) || x.meta.pending || current.seen !== change.after.seen || current.explicit !== change.after.explicit) continue;
      if (change.before.seen === undefined) delete seen[change.key];
      else seen[change.key] = change.before.seen;
      if (change.before.explicit) unread.add(change.key); else unread.delete(change.key);
      applied.push(change.key);
    }
    if (applied.length) this.set({ seen, unread: [...unread] });
    return applied;
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
    this.setHost(host, (s) => ({ ...s, sessions: s.sessions.map((m) => (m.id === id ? { ...m, ...patch } : m)).filter((m) => this.keeps(m)) }));
  }

  /**
   * Pins, archives or renames a session through `chat_update` (`title: ""` goes back to the automatic
   * title). Shown at once and put back when the host refuses. A host without `chat_update`: a pin is
   * kept on this device; renaming and archiving reject with `unsupported`.
   */
  update(host: string, id: string, patch: { title?: string; pinned?: boolean; archived?: boolean }, known?: WsMeta): Promise<void> {
    return this.mutate(sessionKey(host, id), () => this.updateOne(host, id, patch, known));
  }

  private mutate<T>(key: string, run: () => Promise<T>): Promise<T> {
    const previous = this.mutations.get(key);
    const next = previous ? previous.catch(() => undefined).then(run) : run();
    this.mutations.set(key, next);
    const clear = () => { if (this.mutations.get(key) === next) this.mutations.delete(key); };
    void next.then(clear, clear);
    return next;
  }

  /** A scoped receipt for batch operations and undo; serialized with ordinary edits of this row. */
  updateFlag(host: string, id: string, field: "pinned" | "archived", value: boolean, known?: WsMeta, expected?: boolean) {
    return this.mutate(sessionKey(host, id), async () => {
      const x = this.find(host, id);
      const meta = x?.meta ?? (field === "archived" && !value ? known : undefined);
      if (!meta) throw new Error("missing_session");
      const before = field === "pinned" ? (!!meta.pinned || this.snap.pins.includes(sessionKey(host, id))) : !!meta.archived;
      if (expected !== undefined && before !== expected) throw new Error("state_changed");
      if (before !== value) await this.updateOne(host, id, { [field]: value }, meta);
      const current = this.find(host, id);
      const after = current ? (field === "pinned" ? this.isPinned(current) : !!current.meta.archived) : value;
      if (after !== value) throw new Error("state_changed");
      return { before, after, meta: { ...(current?.meta ?? meta), [field]: value } };
    });
  }

  private async updateOne(host: string, id: string, patch: { title?: string; pinned?: boolean; archived?: boolean }, known?: WsMeta): Promise<void> {
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
      // A reply carries a full metadata snapshot. Apply only the fields this edit owns to a
      // listed row, so an undo (or a slow reply) cannot rewind titles and live events.
      const accepted: Partial<WsMeta> = { ...optimistic };
      if (fresh) {
        if (patch.pinned !== undefined) accepted.pinned = fresh.pinned;
        if (patch.archived !== undefined) accepted.archived = fresh.archived;
        if (patch.title !== undefined) { accepted.title = fresh.title; accepted.title_source = fresh.title_source; }
      }
      this.setHost(host, (s) => ({
        ...s,
        update: true,
        sessions: (s.sessions.some((m) => m.id === id)
          ? s.sessions.map((m) => (m.id === id ? { ...m, ...accepted } : m))
          : fresh
            ? [fresh, ...s.sessions]
            : s.sessions
        ).filter((m) => this.keeps(m)),
      }));
    } catch (e) {
      const msg = e && typeof e === "object" && "message" in e && typeof e.message === "string" ? e.message : String(e);
      // Put back what was shown (an archived session that failed to come back stays away).
      if (before && !(patch.archived === false && !listed)) {
        this.setHost(host, (s) => ({ ...s, sessions: s.sessions.some((m) => m.id === id) ? s.sessions.map((m) => {
          if (m.id !== id) return m;
          const restore: Partial<WsMeta> = {};
          if (patch.pinned !== undefined && m.pinned === optimistic.pinned) restore.pinned = before.pinned;
          if (patch.archived !== undefined && m.archived === optimistic.archived) restore.archived = before.archived;
          if (patch.title !== undefined && m.title === optimistic.title && m.title_source === optimistic.title_source) {
            restore.title = before.title; restore.title_source = before.title_source;
          }
          return { ...m, ...restore };
        }) : [before, ...s.sessions] }));
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
