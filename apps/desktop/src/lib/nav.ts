// Where the window is: one route at a time inside the persistent shell, with a Finder-like history
// (back / forward) and a logical parent for when there is no history (a deep link, the first page).
// Pure functions over plain data: only `import type`, so tools/desktop-nav.test.mjs runs it in a vm.

import type { ChatTarget } from "../ui/AgentChat";
import type { Place } from "./workspace";

export type Section = "general" | "agents" | "device" | "network" | "approvals" | "about";
export type ComputerTab = "overview" | "access" | "tools" | "agents";
/** The sidebar rows that are pages of their own. */
export type NavPage = "devices" | "approvals" | "activity" | "pair" | "settings";
export type NavKey = NavPage | { computer: string };

export const SECTIONS: readonly Section[] = ["general", "agents", "device", "network", "approvals", "about"];
export const COMPUTER_TABS: readonly ComputerTab[] = ["overview", "access", "tools", "agents"];

/** The existing workspace's selection, whether it fills the window or sits beside a live screen. */
export interface AgentLocation {
  host?: string;
  session?: string;
  place?: Place | null;
}

export type Route =
  | { page: "welcome" }
  | { page: "devices" }
  | { page: "approvals" }
  /** `host`: opened from a computer page, shows that computer's steps. */
  | { page: "activity"; host?: string }
  | { page: "pair" }
  | { page: "settings"; sec: Section }
  | { page: "computer"; host: string; tab: ComputerTab }
  | { page: "session"; host: string; panel?: "chat"; chat?: ChatTarget | null; workspace?: AgentLocation }
  /** The agents workspace (it takes the whole window, with its own sidebar): one session on a computer,
   *  or the new-session page; `place` pre-fills that page (it does not make a different place). */
  | { page: "agents"; host?: string; session?: string; place?: Place | null };

/** The two halves of the window: the paired computers (the shell) or the agents workspace. */
export type AppMode = "computers" | "agents";

/** `stack[i]` is the current route. */
export interface History {
  stack: Route[];
  i: number;
}

/** Opens a computer's screen with the conversation the reader came from. Only the session id
 *  crosses views: the conversation obtains a fresh attachment ticket from the host. */
export function screenRoute(host: string, session?: string): Route {
  return session ? { page: "session", host, chat: { session } } : { page: "session", host };
}

/** Legacy screen links open their selected conversation in the complete workspace too. */
export function workspaceOfScreen(route: Extract<Route, { page: "session" }>): AgentLocation {
  return route.workspace ?? (route.chat ? { host: route.host, session: route.chat.session } : { host: route.host });
}

/** Keep the picture on the workspace's computer. Session changes on that computer keep its stream. */
export function screenWorkspaceRoute(route: Extract<Route, { page: "session" }>, workspace: AgentLocation): Extract<Route, { page: "session" }> {
  const host = workspace.host ?? workspace.place?.host ?? route.host;
  return { page: "session", host, workspace };
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
 * `mode`: the half the window was last in; with no `view` at all, the agents workspace opens again.
 */
export function parseRoute(
  search: string,
  mem: Memory = DEFAULT_MEMORY,
  mode: AppMode = "computers",
): { route: Route; explicit: boolean; welcomed?: boolean } {
  const q = new URLSearchParams(search);
  const view = q.get("view");
  const host = q.get("host");
  if (view === null && mode === "agents") return { route: { page: "agents" }, explicit: false };
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
    case "agents": {
      // a session needs its computer; without one the session id is dropped
      const session = host ? q.get("session") : null;
      const route: Route = host && session ? { page: "agents", host, session } : host ? { page: "agents", host } : { page: "agents" };
      return { route, explicit: true };
    }
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
      return a.host === (b as typeof a).host
        && (workspaceOfScreen(a).session ?? null) === (workspaceOfScreen(b as typeof a).session ?? null);
    case "agents":
      return (a.host ?? null) === ((b as typeof a).host ?? null) && (a.session ?? null) === ((b as typeof a).session ?? null);
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
    case "agents":
      // a session's parent is the workspace's new-session page, the top of the workspace
      return r.session ? { page: "agents" } : null;
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

/**
 * The window went to the tray: a live view must not go on unseen. A session opened from the agents
 * workspace goes back to it (the session entry is dropped, so › does not reopen the picture); any
 * other session becomes its computer.
 */
export function hidden(h: History, mem: Memory = DEFAULT_MEMORY): History {
  const r = current(h);
  if (r.page !== "session") return h;
  // The combined view keeps its workspace selection. App's hidden-window gate stops the stream;
  // returning to the window resumes that same conversation rather than an older history entry.
  if (r.workspace !== undefined || r.chat?.session) {
    const workspace = workspaceOfScreen(r);
    return replace(h, { page: "agents", ...workspace, host: workspace.host ?? workspace.place?.host ?? r.host });
  }
  if (h.i > 0 && h.stack[h.i - 1].page === "agents") return { stack: h.stack.slice(0, h.i), i: h.i - 1 };
  return replace(h, { page: "computer", host: r.host, tab: mem.tab });
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
    case "agents":
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

/**
 * Where the mode switch's "agents" goes: the workspace route you were last on in this history (the
 * session you came from, a page you went back past), else its new-session page.
 */
export function lastAgents(h: History): Route {
  const workspace = (r: Route): Route | null => {
    if (r.page === "agents" || (r.page === "session" && r.workspace !== undefined)) return r;
    // Computer-page chat_start and older screen links also open the complete workspace. Remember
    // their selection without taking a first-use attachment ticket into the next mount.
    if (r.page === "session" && r.chat?.session) return { page: "agents", host: r.host, session: r.chat.session };
    return null;
  };
  for (let i = h.i; i >= 0; i--) {
    const r = workspace(h.stack[i]);
    if (r) return r;
  }
  for (let i = h.i + 1; i < h.stack.length; i++) {
    const r = workspace(h.stack[i]);
    if (r) return r;
  }
  return { page: "agents" };
}

/**
 * The half of the window a route belongs to, to open there again next time: the workspace or the
 * computers; null for the routes both halves reach (Settings, a live session) and the first run.
 */
export function modeOf(r: Route): AppMode | null {
  switch (r.page) {
    case "agents":
      return "agents";
    case "settings":
    case "session":
    case "welcome":
      return null;
    default:
      return "computers";
  }
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
