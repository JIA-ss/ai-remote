// Where the window is: one route at a time inside the persistent shell, with a Finder-like history
// (back / forward) and a logical parent for when there is no history (a deep link, the first page).
// Pure functions over plain data: only `import type`, so tools/desktop-nav.test.mjs runs it in a vm.

import type { ChatTarget } from "../ui/AgentChat";

export type Section = "general" | "agents" | "device" | "network" | "approvals" | "about";
export type ComputerTab = "overview" | "access" | "tools" | "agents";
/** The sidebar rows that are pages of their own. */
export type NavPage = "devices" | "approvals" | "activity" | "pair" | "settings";
export type NavKey = NavPage | { computer: string };

export const SECTIONS: readonly Section[] = ["general", "agents", "device", "network", "approvals", "about"];
export const COMPUTER_TABS: readonly ComputerTab[] = ["overview", "access", "tools", "agents"];

export type Route =
  | { page: "welcome" }
  | { page: "devices" }
  | { page: "approvals" }
  /** `host`: opened from a computer page, shows that computer's steps. */
  | { page: "activity"; host?: string }
  | { page: "pair" }
  | { page: "settings"; sec: Section }
  | { page: "computer"; host: string; tab: ComputerTab }
  | { page: "session"; host: string; panel?: "chat"; chat?: ChatTarget | null };

/** `stack[i]` is the current route. */
export interface History {
  stack: Route[];
  i: number;
}

/** The last settings section and computer tab, so a sidebar click returns to them. */
export interface Memory {
  sec: Section;
  tab: ComputerTab;
}

export const DEFAULT_MEMORY: Memory = { sec: "general", tab: "overview" };

/**
 * A page's objection to being left (pairing, between "paired" and "safety words checked"): return
 * false to let the shell go now, or true to hold it and call `proceed` once the person agreed.
 */
export type NavGuard = (proceed: () => void) => boolean;
export const HISTORY_LIMIT = 50;

export const isSection = (v: unknown): v is Section => SECTIONS.includes(v as Section);
export const isComputerTab = (v: unknown): v is ComputerTab => COMPUTER_TABS.includes(v as ComputerTab);

/**
 * The route a URL opens (`?view=…`, the deep links screenshots and the e2e scripts use).
 * `explicit`: the URL named a page, so the first-run welcome does not replace it.
 * `welcomed`: `view=welcome` / `view=empty` force the first-run state (undefined: use the stored one).
 */
export function parseRoute(search: string, mem: Memory = DEFAULT_MEMORY): { route: Route; explicit: boolean; welcomed?: boolean } {
  const q = new URLSearchParams(search);
  const view = q.get("view");
  const host = q.get("host");
  switch (view) {
    case "devices":
    case "approvals":
    case "pair":
      return { route: { page: view }, explicit: true };
    case "activity":
      return { route: host ? { page: "activity", host } : { page: "activity" }, explicit: true };
    case "settings": {
      const sec = q.get("sec");
      return { route: { page: "settings", sec: isSection(sec) ? sec : mem.sec }, explicit: true };
    }
    case "computer": {
      if (!host) break;
      const tab = q.get("tab");
      return { route: { page: "computer", host, tab: isComputerTab(tab) ? tab : "overview" }, explicit: true };
    }
    case "session":
      if (!host) break;
      return { route: q.get("panel") === "chat" ? { page: "session", host, panel: "chat" } : { page: "session", host }, explicit: true };
    case "welcome":
      return { route: { page: "welcome" }, explicit: false, welcomed: false };
    case "empty":
      return { route: { page: "devices" }, explicit: false, welcomed: true };
  }
  return { route: { page: "devices" }, explicit: false };
}

/** Whether two routes are the same place (the chat target that opened a session does not count). */
export function same(a: Route, b: Route): boolean {
  if (a.page !== b.page) return false;
  switch (a.page) {
    case "activity":
      return a.host === (b as typeof a).host;
    case "settings":
      return a.sec === (b as typeof a).sec;
    case "computer":
      return a.host === (b as typeof a).host && a.tab === (b as typeof a).tab;
    case "session":
      return a.host === (b as typeof a).host && (a.panel ?? null) === ((b as typeof a).panel ?? null);
    default:
      return true;
  }
}

export const current = (h: History): Route => h.stack[h.i];

/** Goes to `r`: drops the forward history, keeps at most HISTORY_LIMIT entries. The same place again
 *  replaces the entry instead of adding one (clicking the row you are on adds no history). */
export function push(h: History, r: Route): History {
  if (same(current(h), r)) return replace(h, r);
  const stack = [...h.stack.slice(0, h.i + 1), r];
  const drop = Math.max(0, stack.length - HISTORY_LIMIT);
  return { stack: stack.slice(drop), i: stack.length - 1 - drop };
}

/** Swaps the current entry (tabs, settings sections and filters make no history). */
export function replace(h: History, r: Route): History {
  const stack = [...h.stack];
  stack[h.i] = r;
  return { stack, i: h.i };
}

/** Where ‹ goes with no history: the logical parent. */
export function parentOf(r: Route, mem: Memory = DEFAULT_MEMORY): Route | null {
  switch (r.page) {
    case "session":
      return { page: "computer", host: r.host, tab: mem.tab };
    case "activity":
      return r.host ? { page: "computer", host: r.host, tab: mem.tab } : { page: "devices" };
    case "computer":
    case "approvals":
    case "pair":
    case "settings":
      return { page: "devices" };
    default:
      return null;
  }
}

export function back(h: History, mem: Memory = DEFAULT_MEMORY): History {
  if (h.i > 0) return { stack: h.stack, i: h.i - 1 };
  const parent = parentOf(current(h), mem);
  return parent ? replace(h, parent) : h;
}

export function forward(h: History): History {
  return h.i < h.stack.length - 1 ? { stack: h.stack, i: h.i + 1 } : h;
}

export const canBack = (h: History, mem: Memory = DEFAULT_MEMORY): boolean => h.i > 0 || parentOf(current(h), mem) !== null;
export const canForward = (h: History): boolean => h.i < h.stack.length - 1;

/** The window went to the tray: a live view must not go on unseen, so a session becomes its computer. */
export function hidden(h: History, mem: Memory = DEFAULT_MEMORY): History {
  const r = current(h);
  return r.page === "session" ? replace(h, { page: "computer", host: r.host, tab: mem.tab }) : h;
}

/**
 * The route a tray intent asks for (`tray-navigate`): the tray's items all open Settings › This device
 * (with an action for that page); an intent without a view only brought the window back (null: stay).
 */
export function fromTray(p: { view?: string; sec?: string } | null | undefined): Route | null {
  const view = p?.view;
  if (view === "settings") return { page: "settings", sec: isSection(p?.sec) ? p.sec : "device" };
  if (view === "devices" || view === "approvals" || view === "activity" || view === "pair") return { page: view };
  return null;
}

/** The sidebar row a route highlights: a computer's page and its session both light that computer. */
export function navKey(r: Route): NavKey | null {
  switch (r.page) {
    case "computer":
    case "session":
      return { computer: r.host };
    case "welcome":
      return null;
    default:
      return r.page;
  }
}

/** The route a sidebar row (or the session's "back to the list") opens. */
export function routeOfNav(n: NavKey | "home", mem: Memory = DEFAULT_MEMORY): Route {
  if (n === "home" || n === "devices") return { page: "devices" };
  if (typeof n === "object") return { page: "computer", host: n.computer, tab: mem.tab };
  if (n === "settings") return { page: "settings", sec: mem.sec };
  return { page: n };
}

/** What a route teaches the memory: the section and tab you were last on. */
export function remember(mem: Memory, r: Route): Memory {
  if (r.page === "settings" && r.sec !== mem.sec) return { ...mem, sec: r.sec };
  if (r.page === "computer" && r.tab !== mem.tab) return { ...mem, tab: r.tab };
  return mem;
}

/** Reads a stored memory, keeping only known values. */
export function parseMemory(raw: string | null): Memory {
  try {
    const v = raw ? (JSON.parse(raw) as Partial<Memory>) : {};
    return { sec: isSection(v.sec) ? v.sec : DEFAULT_MEMORY.sec, tab: isComputerTab(v.tab) ? v.tab : DEFAULT_MEMORY.tab };
  } catch {
    return DEFAULT_MEMORY;
  }
}

// ---- the sidebar's two states, remembered per context ----

export type SidebarState = "expanded" | "compact";
export type SidebarContext = "shell" | "session";
export type SidebarPref = Partial<Record<SidebarContext, SidebarState>>;

const isSidebarState = (v: unknown): v is SidebarState => v === "expanded" || v === "compact";

export function parseSidebarPref(raw: string | null): SidebarPref {
  try {
    const v = raw ? (JSON.parse(raw) as Record<string, unknown>) : {};
    const out: SidebarPref = {};
    if (isSidebarState(v.shell)) out.shell = v.shell;
    if (isSidebarState(v.session)) out.session = v.session;
    return out;
  } catch {
    return {};
  }
}

/**
 * The sidebar's state in a context: what the person chose there, else the default (pages: expanded,
 * compact in a window narrower than 1100 px; sessions: compact, the stage gets the room).
 */
export function sidebarState(pref: SidebarPref, ctx: SidebarContext, windowWidth: number): SidebarState {
  return pref[ctx] ?? (ctx === "session" || windowWidth < 1100 ? "compact" : "expanded");
}
