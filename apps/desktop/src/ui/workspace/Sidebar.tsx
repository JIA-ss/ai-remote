// The workspace's sidebar (clean-room spec SB, flat variant): the 48 px strip with the window corner and
// the computers / agents switch, a search field that opens the ⌘K panel, "New", then the sessions —
// pinned ones first, then "Sessions" with its "Group and sort" menu over one group per folder on a
// computer (or per day, per state, or none; five rows each, "Show N more") — and a footer with this
// device and Settings. Rows are one line: the title fading out at its end, the branch, and the status
// dot that turns into "…" on hover. A right click or "…" opens the row's menu; a group header has its
// own (new session there, copy the path, collapse the others — also Alt+click).

import { ChevronRight, CodeXml, GitBranch, ListFilter, MoreHorizontal, Plus, Search, Settings } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import { folderName, knownHome } from "../../lib/chat";
import { currentLang, t, type Key } from "../../lib/i18n";
import type { Computer } from "../../lib/store";
import { EMPTY_SELECTION, readLocked as batchReadLocked, runBatch, selectRow, undoBatch, type BatchKind, type BatchResult, type RowSelection } from "../../lib/workspace-batch";
import {
  aggregateDot,
  dateBuckets,
  DEFAULT_LIST,
  groupSessions,
  isAscending,
  isDefaultList,
  isFiltered,
  normalizeList,
  rowsBeforeMore,
  SESSION_JUMP,
  sessionJumpChord,
  sessionJumpNumber,
  shownByStatus,
  sortSessions,
  stateBucket,
  stateBuckets,
  truncateGroup,
  withAscending,
  type DayBucket,
  type GroupBy,
  type ListPrefs,
  type Place,
  type SortMode,
  type StatusFilter,
  type WsItem,
  type WsSnapshot,
} from "../../lib/workspace";
import { cx } from "../util";
import { archive, markDone, readLocked, togglePin, toggleRead, unarchive, type ActionCtx } from "./actions";
import { ModeSwitch, type AppMode } from "./ModeSwitch";
import { ComputerMark, modLabel, PopMenu, StatusDot, useOverflow, viewHeight, workspaceOwnsEvent, type MenuAt, type MenuItem, type MenuLine, type Toast } from "./parts";
import { workspace } from "./useWorkspace";

const COLLAPSED = "ai-remote.ws.collapsed";
/** How the list groups, sorts and filters (SB-46). */
const LIST = "ai-remote.ws.list";
const PINNED_KEY = "pinned";
const ALL_KEY = "all";

/** SB-41: number the rows actually drawn by the current grouping (not rows behind Show more). */
function useSessionJumps(list: RefObject<HTMLDivElement | null>, disabled: boolean, onOpen: (key: string) => void): string[] {
  const [keys, setKeys] = useState<string[]>([]);
  const open = useRef(onOpen);
  open.current = onOpen;
  useEffect(() => {
    setKeys([]);
    if (disabled) return;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let held = false;
    let showing = false;
    const rows = () => [...(list.current?.querySelectorAll<HTMLButtonElement>(".ws-row-main") ?? [])]
      .filter((el) => el.getClientRects().length > 0 && !el.closest("[inert], [aria-hidden=true]"))
      .slice(0, SESSION_JUMP.COUNT);
    const update = () => {
      const next = rows().map((el) => el.closest<HTMLElement>("[data-row-key]")!.dataset.rowKey!);
      setKeys((prev) => prev.length === next.length && prev.every((key, i) => key === next[i]) ? prev : next);
    };
    const hide = () => {
      if (timer) clearTimeout(timer);
      timer = null;
      showing = false;
      setKeys((prev) => prev.length ? [] : prev);
    };
    const editing = () => {
      const a = document.activeElement;
      return a instanceof HTMLElement && (a.isContentEditable || !!a.closest("input, textarea, select, [role=textbox]"));
    };
    const down = (e: globalThis.KeyboardEvent) => {
      const pane = list.current?.closest<HTMLElement>('.ws[data-embedded="true"]');
      if (e.defaultPrevented || (pane && !workspaceOwnsEvent(pane, e)) || e.isComposing || editing() || document.hidden || !sessionJumpChord(e)) {
        held = false;
        hide();
        return;
      }
      const number = sessionJumpNumber(e);
      if (number !== null) {
        if (e.repeat) return;
        // The delay reveals hints; a quick Ctrl+Shift+digit can still open the corresponding row.
        const el = rows()[number - 1];
        if (el) {
          e.preventDefault();
          const key = el.closest<HTMLElement>("[data-row-key]")!.dataset.rowKey!;
          hide();
          el.focus({ preventScroll: true });
          el.scrollIntoView({ block: "nearest" });
          open.current(key);
        }
        return;
      }
      if (e.key !== "Control" && e.key !== "Shift") {
        hide();
        return;
      }
      if (held) return;
      held = true;
      timer = setTimeout(() => {
        timer = null;
        if (!held || editing() || document.hidden) return;
        showing = true;
        update();
      }, SESSION_JUMP.HOLD_MS);
    };
    const up = (e: globalThis.KeyboardEvent) => {
      if (!sessionJumpChord(e)) {
        held = false;
        hide();
      }
    };
    const blur = () => { held = false; hide(); };
    const focus = () => { if (editing()) hide(); };
    let resolution: MediaQueryList;
    const scaleChanged = () => {
      hide();
      resolution.removeEventListener("change", scaleChanged);
      resolution = window.matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
      resolution.addEventListener("change", scaleChanged);
    };
    resolution = window.matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
    resolution.addEventListener("change", scaleChanged);
    const changes = new MutationObserver(() => { if (showing) update(); });
    if (list.current) changes.observe(list.current, { childList: true, subtree: true });
    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    window.addEventListener("pointermove", hide);
    window.addEventListener("wheel", hide, { passive: true });
    window.addEventListener("blur", blur);
    // Browser zoom changes the layout viewport or its visual scale; either removes stale hints.
    window.addEventListener("resize", hide);
    window.visualViewport?.addEventListener("resize", hide);
    document.addEventListener("scroll", hide, true);
    document.addEventListener("focusin", focus);
    document.addEventListener("visibilitychange", blur);
    return () => {
      if (timer) clearTimeout(timer);
      changes.disconnect();
      resolution.removeEventListener("change", scaleChanged);
      window.removeEventListener("keydown", down);
      window.removeEventListener("keyup", up);
      window.removeEventListener("pointermove", hide);
      window.removeEventListener("wheel", hide);
      window.removeEventListener("blur", blur);
      window.removeEventListener("resize", hide);
      window.visualViewport?.removeEventListener("resize", hide);
      document.removeEventListener("scroll", hide, true);
      document.removeEventListener("focusin", focus);
      document.removeEventListener("visibilitychange", blur);
    };
  }, [disabled, list]);
  return keys;
}

function readCollapsed(): Set<string> {
  try {
    return new Set(JSON.parse(localStorage.getItem(COLLAPSED) ?? "[]") as string[]);
  } catch {
    return new Set();
  }
}

function readList(): ListPrefs {
  try {
    return normalizeList(JSON.parse(localStorage.getItem(LIST) ?? "null"));
  } catch {
    return DEFAULT_LIST;
  }
}

/** A session's title as shown ("Untitled session" when it has none at all). */
export function titleOf(x: WsItem): string {
  return workspace.title(x) || t("ws_untitled");
}

/** One group as the list draws it: a folder on a computer (with its `+`), a day, a state, or everything. */
interface Shown {
  key: string;
  label: string;
  title?: string;
  /** A folder group: where `+` starts a session, and whether the computer is this one. */
  place?: Place & { local: boolean };
  items: WsItem[];
}

const dayLabel = (b: DayBucket<WsItem>) =>
  b.kind === "today"
    ? t("ws_today")
    : b.kind === "yesterday"
      ? t("ws_yesterday")
      : b.kind === "older"
        ? t("ws_older")
        : new Date(b.day).toLocaleDateString(currentLang() === "zh" ? "zh-CN" : "en", { month: "short", day: "numeric" });

export interface SidebarProps {
  snap: WsSnapshot;
  computers: Computer[];
  corner: ReactNode;
  mode: AppMode;
  onMode: (m: AppMode) => void;
  /** Requests waiting on the computers side, as a sentence (the mode switch flags it). */
  attention?: string;
  /** The open session (`host/id`), or null on the new-session page. */
  current: string | null;
  onOpen: (host: string, id: string) => void;
  onNew: (place?: Place) => void;
  onPalette: () => void;
  onPair: () => void;
  onSettings: () => void;
  onArchived: (key: string) => void;
  device: string;
  toast: (t: Omit<Toast, "id">) => void;
  /** A row to rename now (F2 / ⌘⌥R from elsewhere), by key. */
  renaming: string | null;
  setRenaming: (key: string | null) => void;
}

export function Sidebar(p: SidebarProps) {
  const [prefs, setPrefsState] = useState(readList);
  /** Changes the settings from what they are now (two choices in one tick both count). */
  const setPrefs = (change: Partial<ListPrefs> | ((p: ListPrefs) => ListPrefs)) =>
    setPrefsState((prev) => {
      const next = typeof change === "function" ? change(prev) : { ...prev, ...change };
      try {
        localStorage.setItem(LIST, JSON.stringify(next));
      } catch {
        // Kept for this run only.
      }
      return next;
    });
  // Archived sessions are only asked for while the list shows them (SB-11).
  useEffect(() => workspace.setArchived(prefs.status !== "active"), [prefs.status]);
  const all = useMemo(() => workspace.items(), [p.snap]);
  const items = useMemo(() => all.filter((x) => shownByStatus(x.meta, prefs.status)), [all, prefs.status]);
  const { pinned, groups } = useMemo(() => {
    const pinnedOf = (x: WsItem) => workspace.isPinned(x);
    const g = groupSessions(items, { pinned: pinnedOf, title: titleOf, home: knownHome, sort: prefs.sort, reversed: prefs.reversed });
    if (prefs.groupBy === "project") {
      const shown: Shown[] = g.groups.map((x) => ({
        key: x.key,
        label: x.label,
        title: x.local ? x.folder : t("ws_group_where", { host: x.host, path: x.folder }),
        place: { host: x.host, cwd: x.folder, local: x.local },
        items: x.items,
      }));
      return { pinned: g.pinned, groups: shown };
    }
    const rest = sortSessions(
      items.filter((x) => !pinnedOf(x)),
      prefs.sort,
      titleOf,
      prefs.reversed,
    );
    let shown: Shown[];
    if (prefs.groupBy === "date") {
      const by = prefs.sort === "created" ? "started_at" : "last_activity";
      shown = dateBuckets(rest, Date.now(), by, prefs.sort !== "alpha" && isAscending(prefs)).map((b) => ({ key: `date:${b.key}`, label: dayLabel(b), items: b.items }));
    } else if (prefs.groupBy === "state") {
      shown = stateBuckets(rest, (x) => stateBucket(x.meta, workspace.dot(x))).map((b) => ({ key: `state:${b.key}`, label: t(`ws_state_${b.key}` as Key), items: b.items }));
    } else {
      shown = rest.length ? [{ key: ALL_KEY, label: t("ws_sessions_head"), items: rest }] : [];
    }
    return { pinned: g.pinned, groups: shown };
  }, [items, p.snap.pins, prefs]);
  const [collapsed, setCollapsed] = useState(readCollapsed);
  const [opened, setOpened] = useState<Set<string>>(new Set());
  const [menu, setMenu] = useState<{ key: string; at: MenuAt } | null>(null);
  const [groupMenu, setGroupMenu] = useState<{ key: string; at: MenuAt } | null>(null);
  const [listMenu, setListMenu] = useState<MenuAt | null>(null);
  const [focusId, setFocusId] = useState<string | null>(null);
  const list = useRef<HTMLDivElement>(null);
  const side = useRef<HTMLElement>(null);
  const [selection, setSelection] = useState<RowSelection>(EMPTY_SELECTION);
  const [batchBusy, setBatchBusy] = useState(false);
  const batchQueue = useRef(Promise.resolve());
  const clearSelection = () => setSelection((s) => s.keys.length ? EMPTY_SELECTION : s);
  useEffect(() => {
    const valid = new Set(items.map((x) => x.key));
    setSelection((s) => {
      const keys = s.keys.filter((key) => valid.has(key));
      return keys.length === s.keys.length ? s : { keys, anchor: s.anchor && valid.has(s.anchor) ? s.anchor : null };
    });
  }, [items]);
  useEffect(() => {
    const outside = (e: PointerEvent) => {
      if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey || !(e.target instanceof Element)) return;
      if ((list.current?.contains(e.target) && e.target.closest("[data-row-key]"))
        || (side.current?.contains(e.target) && e.target.closest(".ws-batch-menu"))) return;
      setSelection((s) => s.keys.length ? EMPTY_SELECTION : s);
    };
    document.addEventListener("pointerdown", outside, true);
    return () => document.removeEventListener("pointerdown", outside, true);
  }, []);
  useEffect(() => {
    if (!selection.keys.length) return;
    const escape = (e: globalThis.KeyboardEvent) => {
      if (!side.current) return;
      const pane = side.current.closest<HTMLElement>('.ws[data-embedded="true"]');
      if (e.defaultPrevented || e.isComposing || (pane && !workspaceOwnsEvent(pane, e))) return;
      // Single-row shortcuts never silently act on one member of a multi-selection. Menu
      // letters stay local to PopMenu; Ctrl+Shift+digits still belong to the jump handler.
      if (selection.keys.length > 1 && ((e.key === "F2" && !e.metaKey && !e.ctrlKey && !e.altKey)
        || ((e.metaKey || e.ctrlKey) && e.altKey && ["KeyP", "KeyU", "KeyA", "KeyR"].includes(e.code)))) {
        e.preventDefault(); e.stopPropagation(); return;
      }
      if (e.key !== "Escape") return;
      e.preventDefault(); e.stopPropagation();
      setSelection(EMPTY_SELECTION); setMenu(null); setGroupMenu(null); setListMenu(null);
    };
    window.addEventListener("keydown", escape, true);
    return () => window.removeEventListener("keydown", escape, true);
  }, [selection.keys.length]);
  // SB-32: a card beside the row the pointer rests on (500 ms; once one shows, the next row's follows at
  // once), gone 200 ms after the pointer leaves; not while a menu is open or a row is being renamed.
  const [card, setCard] = useState<{ key: string; rect: DOMRect } | null>(null);
  const cardTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const quiet = !!menu || !!groupMenu || !!listMenu || !!p.renaming;
  const hoverRow = (key: string | null, el?: HTMLElement) => {
    if (cardTimer.current) clearTimeout(cardTimer.current);
    if (key && el && !quiet) cardTimer.current = setTimeout(() => setCard({ key, rect: el.getBoundingClientRect() }), card ? 0 : 500);
    else cardTimer.current = setTimeout(() => setCard(null), key ? 0 : 200);
  };
  useEffect(() => {
    if (quiet) setCard(null);
  }, [quiet]);
  useEffect(() => () => {
    if (cardTimer.current) clearTimeout(cardTimer.current);
  }, []);
  const byKey = useMemo(() => new Map(all.map((x) => [x.key, x])), [all]);
  const jumps = useSessionJumps(list, quiet, (key) => {
    const item = byKey.get(key);
    if (item) { clearSelection(); p.onOpen(item.host, item.meta.id); }
  });
  const computer = (host: string) => p.computers.find((c) => c.host.name === host) ?? null;
  const ctx: ActionCtx = { toast: p.toast, onArchived: p.onArchived };
  const selectedItems = selection.keys.flatMap((key) => byKey.get(key) ? [byKey.get(key)!] : []);
  const bulk = selectedItems.length > 1 && !!menu && selection.keys.includes(menu.key);
  const selectAllKeys = [...pinned, ...groups.flatMap((g) => g.items)].map((x) => x.key);
  const folded = (pinned.length > 0 && collapsed.has(PINNED_KEY)) || groups.some((g) => g.key !== ALL_KEY && g.items.length > 0 && collapsed.has(g.key));
  const selectAllNote = folded ? t("ws_select_all_expand") : selectAllKeys.length < 2 ? t("ws_select_all_min") : undefined;
  const selectAll = () => {
    if (selectAllNote) { p.toast({ text: selectAllNote }); return; }
    setSelection({ keys: selectAllKeys, anchor: selectAllKeys[0] ?? null });
  };
  const batchError = (error: string) => error === "busy_session" ? t("ws_batch_read_locked")
    : error === "missing_session" ? t("ws_batch_missing") : error === "state_changed" ? t("ws_batch_changed")
    : error === "unsupported" || /unknown tool '?chat_/i.test(error) ? t("ws_batch_unsupported") : error;
  const dispatchBatch = (job: () => Promise<BatchResult>, label: string, offerUndo: boolean) => {
    clearSelection();
    batchQueue.current = batchQueue.current.catch(() => undefined).then(async () => {
      setBatchBusy(true);
      try {
        const result = await job();
        for (const change of result.successes) if (change.field === "archived" && change.before !== change.after && change.after) p.onArchived(change.item.key);
        const changes = result.successes.filter((c) => c.field === "read" ? c.before.seen !== c.after.seen || c.before.explicit !== c.after.explicit : c.before !== c.after);
        const detail = result.failures.slice(0, 3).map((f) => `${f.title}: ${batchError(f.error)}`).join("; ");
        let undone = false;
        p.toast({
          text: `${label}: ${t(offerUndo ? "ws_batch_result" : "ws_batch_undone", { ok: result.successes.length, failed: result.failures.length })}${detail ? ` — ${detail}` : ""}${result.failures.length > 3 ? ` (${t("ws_batch_more_errors", { n: result.failures.length - 3 })})` : ""}`,
          tone: result.failures.length ? "error" : undefined,
          action: offerUndo && changes.length ? { label: t("ws_undo"), run: () => {
            if (undone) return;
            undone = true;
            dispatchBatch(() => undoBatch(workspace, changes), label, false);
          } } : undefined,
        });
      } catch (error) { p.toast({ text: t("ws_failed", { error: String((error as Error)?.message ?? error) }), tone: "error" }); }
      finally { setBatchBusy(false); }
    });
  };
  const batchItems = (): MenuLine[] => {
    const n = selectedItems.length;
    const pinned = selectedItems.every((x) => workspace.isPinned(x));
    const read = !selectedItems.some((x) => workspace.unread(x));
    const archived = selectedItems.every((x) => !!x.meta.archived);
    const action = (kind: BatchKind, value: boolean, label: string, letter: string, locked = false): MenuLine => ({
      key: `batch-${kind}`, label, letter, disabled: batchBusy || locked,
      note: batchBusy ? t("ws_batch_busy") : locked ? t("ws_batch_read_locked") : undefined,
      run: () => { const chosen = selectedItems.map((x) => ({ ...x, meta: { ...x.meta } })); dispatchBatch(() => runBatch(workspace, chosen, kind, value), label, true); },
    });
    return [
      action("pin", !pinned, t(pinned ? "ws_batch_unpin" : "ws_batch_pin", { n }), "p"),
      action("read", !read, t(read ? "ws_batch_unread" : "ws_batch_read", { n }), "u", selectedItems.some(batchReadLocked)),
      "sep", action("archive", !archived, t(archived ? "ws_batch_unarchive" : "ws_batch_archive", { n }), "a"),
    ];
  };

  const saveCollapsed = (next: Set<string>) => {
    setCollapsed(next);
    try {
      localStorage.setItem(COLLAPSED, JSON.stringify([...next]));
    } catch {
      // Kept for this run only.
    }
  };
  /** Folds or opens a group; `others` (Alt+click, SB-18) folds every other group and opens this one. */
  const toggleGroup = (key: string, others = false) => {
    const next = new Set(collapsed);
    if (others) {
      for (const g of groups) if (g.key !== key) next.add(g.key);
      if (pinned.length && key !== PINNED_KEY) next.add(PINNED_KEY);
      next.delete(key);
    } else if (next.has(key)) next.delete(key);
    else next.add(key);
    saveCollapsed(next);
  };

  // The open session's group opens when it is folded (SB-18), so the row is in view.
  useEffect(() => {
    if (!p.current) return;
    const el = list.current?.querySelector<HTMLElement>(`[data-row-key="${CSS.escape(p.current)}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [p.current]);

  // Roving focus (SB-40): ↑ / ↓ over rows and group headers, Home / End, F2 renames.
  const onListKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.target instanceof HTMLInputElement) return;
    if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "a") { e.preventDefault(); selectAll(); return; }
    const nav = [...(list.current?.querySelectorAll<HTMLElement>("[data-ws-nav]") ?? [])];
    const i = nav.indexOf(document.activeElement as HTMLElement);
    const from = nav[i]?.closest<HTMLElement>("[data-row-key]")?.dataset.rowKey ?? null;
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    let next = -1;
    if (e.key === "ArrowDown") next = (i + 1) % nav.length;
    else if (e.key === "ArrowUp") next = (i - 1 + nav.length) % nav.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = nav.length - 1;
    else if (e.key === "F2") {
      const key = (document.activeElement as HTMLElement | null)?.closest<HTMLElement>("[data-row-key]")?.dataset.rowKey;
      if (key) {
        e.preventDefault();
        clearSelection();
        p.setRenaming(key);
      }
      return;
    }
    if (next < 0 || !nav.length) return;
    e.preventDefault();
    nav[next].focus();
    nav[next].scrollIntoView({ block: "nearest" });
    if (e.shiftKey) {
      const key = nav[next].closest<HTMLElement>("[data-row-key]")?.dataset.rowKey;
      if (key) setSelection((s) => selectRow(s.anchor ? s : { ...s, anchor: from }, key, [...list.current!.querySelectorAll<HTMLElement>("[data-row-key]")].map((r) => r.dataset.rowKey!), false, true));
    }
  };
  // One Tab stop for the list: the last focused item, else the first header.
  const navIds = useMemo(() => {
    const ids = new Set<string>();
    if (pinned.length) ids.add(`group:${PINNED_KEY}`);
    for (const g of groups) ids.add(`group:${g.key}`).add(`more:${g.key}`);
    for (const x of items) ids.add(`row:${x.key}`);
    return ids;
  }, [items, pinned, groups]);
  const firstRow = groups[0]?.key === ALL_KEY && groups[0].items[0] ? `row:${groups[0].items[0].key}` : null;
  const firstNav = pinned.length ? `group:${PINNED_KEY}` : groups[0] ? (firstRow ?? `group:${groups[0].key}`) : null;
  const stop = focusId && navIds.has(focusId) ? focusId : firstNav;
  const navIndex = (id: string) => (id === stop ? 0 : -1);

  const menuItems = (x: WsItem): MenuLine[] => {
    const host = p.snap.hosts[x.host];
    const old = host?.update === false;
    const oldNote = t("ws_old_host", { host: x.host });
    const dot = workspace.dot(x);
    const unread = workspace.unread(x);
    if (x.meta.archived) {
      return [
        { key: "rename", label: t("ws_rename"), letter: "r", disabled: old, note: oldNote, run: () => p.setRenaming(x.key) },
        "sep",
        { key: "unarchive", label: t("ws_unarchive"), letter: "a", disabled: old, note: oldNote, run: () => unarchive(x, ctx) },
      ];
    }
    const read: MenuItem =
      // SB-26: a failed session can be put aside; it then takes the read letter (it is not running).
      dot === "failed"
        ? { key: "done", label: t("ws_mark_done"), letter: "u", run: () => markDone(x, ctx) }
        : { key: "read", label: t(unread ? "ws_mark_read" : "ws_mark_unread"), letter: "u", disabled: readLocked(x), note: t("ws_read_busy"), run: () => toggleRead(x) };
    return [
      { key: "pin", label: t(workspace.isPinned(x) ? "ws_unpin" : "ws_pin"), letter: "p", run: () => togglePin(x, ctx) },
      read,
      { key: "rename", label: t("ws_rename"), letter: "r", disabled: old, note: oldNote, run: () => p.setRenaming(x.key) },
      "sep",
      { key: "archive", label: t("ws_archive"), letter: "a", disabled: old, note: oldNote, run: () => archive(x, ctx) },
      ...(x.meta.state === "ended" ? [] : [endItem(x)]),
    ];
  };
  const [confirmEnd, setConfirmEnd] = useState<string | null>(null);
  const endItem = (x: WsItem): MenuItem => ({
    key: "end",
    label: confirmEnd === x.key ? t("ws_end_confirm") : t("ws_end"),
    danger: true,
    keep: confirmEnd !== x.key,
    run: () => {
      if (confirmEnd !== x.key) setConfirmEnd(x.key);
      else {
        setConfirmEnd(null);
        void workspace.end(x.host, x.meta.id).catch((e) => {
          const msg = e instanceof Error ? e.message : String(e);
          p.toast({ text: t("ws_failed", { error: msg }), tone: "error" });
        });
      }
    },
  });
  const menuOf = menu ? byKey.get(menu.key) : null;

  // A group header's menu (SB-45): a new session there, copy the folder's path, fold the others.
  const groupMenuItems = (key: string): MenuLine[] => {
    const g = groups.find((x) => x.key === key);
    const lines: MenuLine[] = [];
    if (g?.place) {
      const place = g.place;
      const offline = computer(place.host)?.online !== true;
      lines.push(
        { key: "new", label: t("ws_new_in", { folder: g.label }), disabled: offline, note: t("ws_new_offline", { host: place.host }), run: () => p.onNew({ host: place.host, cwd: place.cwd }) },
        {
          key: "copy",
          label: t("ws_copy_path"),
          run: () => void navigator.clipboard?.writeText(place.cwd).then(() => p.toast({ text: t("ws_copied", { path: place.cwd }) }), () => undefined),
        },
        "sep",
      );
    }
    const keys = [...(pinned.length ? [PINNED_KEY] : []), ...groups.map((x) => x.key)];
    lines.push(
      { key: "fold", label: t("ws_fold_others"), disabled: keys.every((k) => k === key || collapsed.has(k)), run: () => toggleGroup(key, true) },
      { key: "unfold", label: t("ws_expand_all"), disabled: !keys.some((k) => collapsed.has(k)), run: () => saveCollapsed(new Set()) },
    );
    return lines;
  };

  // "Group and sort" (SB-12): grouping, order and which sessions show — choices that keep the menu
  // open; restoring the defaults is off while they are the defaults.
  const listMenuItems = (): MenuLine[] => {
    const choice = (key: string, label: string, on: boolean, run: () => void): MenuItem => ({ key, label, checked: on, keep: true, run });
    const groupBy = (v: GroupBy, k: Key) => choice(`g-${v}`, t(k), prefs.groupBy === v, () => setPrefs({ groupBy: v }));
    const sortBy = (v: SortMode, k: Key) => choice(`s-${v}`, t(k), prefs.sort === v, () => setPrefs({ sort: v, reversed: false }));
    const status = (v: StatusFilter, k: Key) => choice(`f-${v}`, t(k), prefs.status === v, () => setPrefs({ status: v }));
    const asc = isAscending(prefs);
    return [
      { key: "select-all", label: t("ws_select_all"), disabled: !!selectAllNote, note: selectAllNote, run: selectAll },
      "sep",
      { heading: t("ws_group_by") },
      groupBy("project", "ws_group_project"),
      groupBy("date", "ws_group_date"),
      groupBy("state", "ws_group_state"),
      groupBy("none", "ws_group_none"),
      "sep",
      { heading: t("ws_sort_by") },
      sortBy("recency", "ws_sort_recency"),
      sortBy("created", "ws_sort_created"),
      sortBy("alpha", "ws_sort_alpha"),
      "sep",
      choice("desc", t("ws_order_desc"), !asc, () => setPrefs((q) => withAscending(q, false))),
      choice("asc", t("ws_order_asc"), asc, () => setPrefs((q) => withAscending(q, true))),
      "sep",
      { heading: t("ws_show_status") },
      status("active", "ws_status_active"),
      status("archived", "ws_status_archived"),
      status("all", "ws_status_all"),
      "sep",
      { key: "reset", label: t("ws_list_reset"), disabled: isDefaultList(prefs), note: t("ws_list_reset_none"), run: () => setPrefs(() => DEFAULT_LIST) },
    ];
  };

  const row = (x: WsItem, opts: { lead?: boolean; env?: boolean } = {}) => (
    <Row
      key={x.key}
      item={x}
      lead={opts.lead}
      jump={jumps.indexOf(x.key) + 1 || undefined}
      env={opts.env && !x.local ? (computer(x.host) ?? { os: null, online: x.online }) : null}
      selected={x.key === p.current}
      multiSelected={selection.keys.includes(x.key)}
      menuOpen={menu?.key === x.key}
      renaming={p.renaming === x.key}
      tabIndex={navIndex(`row:${x.key}`)}
      onFocus={() => setFocusId(`row:${x.key}`)}
      onHover={(el) => hoverRow(el ? x.key : null, el ?? undefined)}
      onOpen={(e) => {
        if (e.metaKey || e.ctrlKey || e.shiftKey) {
          e.preventDefault();
          const order = [...(list.current?.querySelectorAll<HTMLElement>("[data-row-key]") ?? [])].map((r) => r.dataset.rowKey!);
          setSelection((s) => selectRow(s.anchor ? s : { ...s, anchor: p.current }, x.key, order, e.metaKey || e.ctrlKey, e.shiftKey));
        } else { clearSelection(); p.onOpen(x.host, x.meta.id); }
      }}
      onMenu={(at) => {
        if (!selection.keys.includes(x.key)) clearSelection();
        setConfirmEnd(null);
        setMenu({ key: x.key, at });
      }}
      onRenamed={(title) => {
        p.setRenaming(null);
        if (title === null) return;
        void workspace.update(x.host, x.meta.id, { title }).catch((e) => {
          const msg = e instanceof Error ? e.message : String(e);
          p.toast({ text: msg === "unsupported" ? t("ws_old_host", { host: x.host }) : t("ws_failed", { error: msg }), tone: "error" });
        });
      }}
    />
  );

  const noComputers = p.computers.length === 0;
  const loading = !all.length && p.computers.some((c) => c.online === true && (!p.snap.hosts[c.host.name] || p.snap.hosts[c.host.name].status === "loading"));
  const notes = p.computers
    .map((c) => ({ c, h: p.snap.hosts[c.host.name] }))
    .filter(({ c, h }) => c.online === true && h && (h.status === "denied" || h.status === "unsupported" || (h.status === "error" && !h.sessions.length)));
  const filtered = isFiltered(prefs);
  // SB-47: off the folder grouping (where the header names the computer), rows from other computers carry its mark.
  const envRows = prefs.groupBy !== "project";
  const listLabel = filtered ? t("ws_group_sort_on") : t("ws_group_sort");

  return (
    <nav ref={side} className="ws-side" aria-label={t("ws_sidebar")}>
      <div className="ws-side-top" data-tauri-drag-region>
        {p.corner}
        <div className="ws-drag" data-tauri-drag-region />
        <ModeSwitch mode={p.mode} onMode={p.onMode} attention={p.attention} />
      </div>
      <button type="button" className="ws-search" onMouseDown={(e) => e.preventDefault()} onClick={p.onPalette}>
        <span className="ws-lead">
          <Search size={15} strokeWidth={1.5} />
        </span>
        <span className="ws-search-label">{t("ws_search")}</span>
        <span className="ws-search-keys">{modLabel}K</span>
      </button>
      <button
        type="button"
        className={cx("ws-nav", p.current === null && "on")}
        aria-current={p.current === null ? "page" : undefined}
        title={`${t("ws_new_session")} (${modLabel}N)`}
        onClick={() => p.onNew()}
      >
        <span className="ws-lead">
          <Plus size={16} strokeWidth={1.5} />
        </span>
        <span>{t("ws_new")}</span>
      </button>
      <div className="ws-list" ref={list} onKeyDown={onListKey} onScroll={() => card && setCard(null)} role="list" aria-label={t("ws_sessions")}>
        {noComputers ? (
          // the new-session page carries the one way to pair; the list only says what will appear here
          <div className="ws-list-note">{t("ws_empty")}</div>
        ) : loading ? (
          <div className="ws-skel" aria-label={t("ws_loading")}>
            <i style={{ width: "75%" }} />
            <i style={{ width: "50%" }} />
          </div>
        ) : !all.length && !notes.length && !filtered ? (
          <div className="ws-list-note">{t("ws_empty")}</div>
        ) : null}
        {pinned.length > 0 && (
          <Section
            id={PINNED_KEY}
            label={t("ws_pinned")}
            collapsed={collapsed.has(PINNED_KEY)}
            onToggle={(others) => toggleGroup(PINNED_KEY, others)}
            onMenu={(at) => setGroupMenu({ key: PINNED_KEY, at })}
            dot={aggregateDot(pinned.map((x) => workspace.dot(x)))}
            tabIndex={navIndex(`group:${PINNED_KEY}`)}
            onFocus={() => setFocusId(`group:${PINNED_KEY}`)}
          >
            {(collapsed.has(PINNED_KEY) ? pinned.filter((x) => x.key === p.current) : pinned).map((x) => row(x, { lead: true }))}
          </Section>
        )}
        {(all.length > 0 || filtered) && !noComputers && (
          <div className="ws-list-head" data-open={listMenu ? "" : undefined}>
            <span className="ws-list-head-label">{selection.keys.length ? t("ws_selection_count", { n: selection.keys.length }) : t("ws_sessions_head")}</span>
            <button
              type="button"
              className={cx("ws-list-filter", filtered && "on")}
              tabIndex={-1}
              aria-label={listLabel}
              title={listLabel}
              aria-expanded={!!listMenu}
              onClick={(e) => setListMenu(listMenu ? null : { anchor: e.currentTarget.getBoundingClientRect() })}
            >
              <ListFilter size={14} strokeWidth={1.5} />
            </button>
          </div>
        )}
        {filtered && !groups.length && (
          <div className="ws-filter-empty">
            <p>{t("ws_no_match")}</p>
            <button type="button" className="ws-link" onClick={() => setPrefs({ status: "active" })}>
              {t("ws_show_all")}
            </button>
          </div>
        )}
        {groups.map((g) => {
          const folded = collapsed.has(g.key);
          const keep = new Set(p.current ? [p.current] : []);
          const limit = rowsBeforeMore(prefs.groupBy);
          const cut = truncateGroup(g.items, limit, keep, opened.has(g.key), (x) => workspace.dot(x) === "awaiting");
          const place = g.place;
          const c = place ? computer(place.host) : null;
          const offline = c?.online !== true;
          const flat = g.key === ALL_KEY;
          const rows = folded && !flat ? g.items.filter((x) => x.key === p.current) : cut.shown;
          const more = (flat || !folded) && (cut.hidden.length > 0 || opened.has(g.key)) && g.items.length > limit && (
            <button
              type="button"
              className="ws-more"
              data-ws-nav
              tabIndex={navIndex(`more:${g.key}`)}
              onFocus={() => setFocusId(`more:${g.key}`)}
              onClick={() =>
                setOpened((prev) => {
                  const next = new Set(prev);
                  if (next.has(g.key)) next.delete(g.key);
                  else next.add(g.key);
                  return next;
                })
              }
            >
              <span>{opened.has(g.key) ? t("ws_show_less") : t(cut.hidden.length === 1 ? "ws_show_more_one" : "ws_show_more_many", { n: cut.hidden.length })}</span>
              {cut.hiddenNeedsYou && !opened.has(g.key) && <StatusDot kind="awaiting" />}
            </button>
          );
          // Not grouped: the rows sit right under "Sessions", without a header of their own.
          if (flat) {
            return (
              <div key={g.key} className="ws-sec ws-sec-flat" data-group={g.key}>
                <div className="ws-sec-rows" role="group" aria-label={g.label}>
                  {rows.map((x) => row(x, { env: envRows }))}
                  {more}
                </div>
              </div>
            );
          }
          return (
            <Section
              key={g.key}
              id={g.key}
              label={g.label}
              title={g.title}
              collapsed={folded}
              onToggle={(others) => toggleGroup(g.key, others)}
              onMenu={(at) => setGroupMenu({ key: g.key, at })}
              dot={aggregateDot(g.items.map((x) => workspace.dot(x)))}
              tabIndex={navIndex(`group:${g.key}`)}
              onFocus={() => setFocusId(`group:${g.key}`)}
              trailing={
                place && (
                  <>
                    {!place.local && (
                      <span className="ws-gh-env" title={g.title}>
                        <ComputerMark os={c?.os ?? null} online={c?.online ?? null} />
                      </span>
                    )}
                    <button
                      type="button"
                      className="ws-gh-add"
                      tabIndex={-1}
                      disabled={offline}
                      aria-label={offline ? t("ws_new_offline", { host: place.host }) : t("ws_new_in", { folder: g.label })}
                      title={offline ? t("ws_new_offline", { host: place.host }) : t("ws_new_in", { folder: g.label })}
                      onClick={() => p.onNew({ host: place.host, cwd: place.cwd })}
                    >
                      <Plus size={15} strokeWidth={1.5} />
                    </button>
                  </>
                )
              }
            >
              {rows.map((x) => row(x, { env: envRows }))}
              {more}
            </Section>
          );
        })}
        {notes.length > 0 && (
          <div className="ws-host-notes">
            {notes.map(({ c, h }) => (
              <div key={c.host.name} className="ws-host-note">
                {h.status === "denied"
                  ? t("ws_host_denied", { host: c.host.name })
                  : h.status === "unsupported"
                    ? t("ws_host_old", { host: c.host.name })
                    : t("ws_host_error", { host: c.host.name, error: h.error ?? "" })}
              </div>
            ))}
          </div>
        )}
      </div>
      <div className="ws-foot">
        <span className="ws-foot-me" title={t("ws_this_device")}>
          <ComputerMark os={null} online={true} />
          <span className="ws-foot-name">{p.device}</span>
        </span>
        <button type="button" className="ws-ib" onClick={p.onSettings} aria-label={t("ws_settings")} title={t("ws_settings")}>
          <Settings size={15} strokeWidth={1.5} />
        </button>
      </div>
      {menu && menuOf && (
        <PopMenu at={menu.at} className={bulk ? "ws-batch-menu" : undefined} label={bulk ? t("ws_batch_menu", { n: selectedItems.length }) : t("ws_session_menu", { title: titleOf(menuOf) })} items={bulk ? batchItems() : menuItems(menuOf)} onClose={() => setMenu(null)} />
      )}
      {groupMenu && (
        <PopMenu
          at={groupMenu.at}
          label={t("ws_group_menu", { group: groupMenu.key === PINNED_KEY ? t("ws_pinned") : (groups.find((g) => g.key === groupMenu.key)?.label ?? "") })}
          items={groupMenuItems(groupMenu.key)}
          onClose={() => setGroupMenu(null)}
        />
      )}
      {listMenu && <PopMenu at={listMenu} label={listLabel} items={listMenuItems()} onClose={() => setListMenu(null)} />}
      {card && byKey.get(card.key) && <RowCard item={byKey.get(card.key) as WsItem} rect={card.rect} />}
    </nav>
  );
}

/**
 * A section: its header (a fold button, the badge and trailing controls) and its rows (SB-16 / SB-18).
 * Alt+click folds the other groups; a right click opens the group's menu (SB-45).
 */
function Section({
  id,
  label,
  title,
  collapsed,
  onToggle,
  onMenu,
  dot,
  trailing,
  tabIndex,
  onFocus,
  children,
}: {
  id: string;
  label: string;
  title?: string;
  collapsed: boolean;
  onToggle: (others: boolean) => void;
  onMenu: (at: MenuAt) => void;
  dot: ReturnType<typeof aggregateDot>;
  trailing?: ReactNode;
  tabIndex?: number;
  onFocus: () => void;
  children: ReactNode;
}) {
  const ref = useOverflow<HTMLSpanElement>(label);
  return (
    <div className="ws-sec" data-collapsed={collapsed ? "" : undefined} data-group={id}>
      <div
        className="ws-gh"
        onContextMenu={(e) => {
          e.preventDefault();
          onMenu({ x: e.clientX, y: e.clientY });
        }}
      >
        <button
          type="button"
          className="ws-gh-toggle"
          data-ws-nav
          data-group-toggle
          aria-expanded={!collapsed}
          title={title}
          tabIndex={tabIndex}
          onFocus={onFocus}
          onClick={(e) => onToggle(e.altKey)}
        >
          <span className="ws-gh-label" ref={ref}>
            {label}
          </span>
          <ChevronRight className="ws-gh-caret" size={12} strokeWidth={2} />
        </button>
        <span className="ws-gh-badge">{collapsed && dot && <StatusDot kind={dot} />}</span>
        {trailing}
      </div>
      <div className="ws-sec-rows" role="group" aria-label={label}>
        {children}
      </div>
    </div>
  );
}

function Row({
  item,
  lead,
  jump,
  env,
  selected,
  multiSelected,
  menuOpen,
  renaming,
  tabIndex,
  onFocus,
  onHover,
  onOpen,
  onMenu,
  onRenamed,
}: {
  item: WsItem;
  lead?: boolean;
  jump?: number;
  /** SB-47: the computer's mark at the row's end (when no dot is there), off the folder grouping. */
  env: Pick<Computer, "os" | "online"> | null;
  selected: boolean;
  multiSelected: boolean;
  menuOpen: boolean;
  renaming: boolean;
  tabIndex?: number;
  onFocus: () => void;
  /** The pointer came onto the row (its element) or left it (null). */
  onHover: (el: HTMLElement | null) => void;
  onOpen: (e: React.MouseEvent<HTMLButtonElement>) => void;
  onMenu: (at: MenuAt) => void;
  onRenamed: (title: string | null) => void;
}) {
  const title = titleOf(item);
  const dot = workspace.dot(item);
  const ref = useOverflow<HTMLSpanElement>(title);
  const branch = item.meta.branch?.trim();
  const mark = !dot && env ? env : null;
  const decorated = !!dot || !!branch || !!mark;
  return (
    <div
      className="ws-row"
      role="listitem"
      data-row-key={item.key}
      data-selected={selected ? "" : undefined}
      data-multi-selected={multiSelected ? "" : undefined}
      data-menu-open={menuOpen ? "" : undefined}
      data-decorated={decorated ? "" : undefined}
      data-archived={item.meta.archived ? "" : undefined}
      onPointerEnter={(e) => e.pointerType === "mouse" && onHover(e.currentTarget)}
      onPointerLeave={() => onHover(null)}
      onPointerDown={() => onHover(null)}
      onContextMenu={(e) => {
        e.preventDefault();
        onMenu({ x: e.clientX, y: e.clientY });
      }}
    >
      {renaming ? (
        <RenameField initial={item.meta.title?.trim() || title} onDone={onRenamed} />
      ) : (
        <button
          type="button"
          className="ws-row-main"
          data-ws-nav
          tabIndex={tabIndex}
          onFocus={onFocus}
          onClick={onOpen}
          aria-current={selected ? "page" : undefined}
          aria-pressed={multiSelected}
          aria-description={item.meta.archived ? t("ws_archived_row") : undefined}
        >
          {(lead || jump) && (
            <span className="ws-row-lead">
              {jump ? <kbd className="ws-jump-key" aria-hidden>{jump}</kbd> : <CodeXml size={14} strokeWidth={1.5} />}
            </span>
          )}
          <span className="ws-row-title" ref={ref}>
            {title}
          </span>
        </button>
      )}
      {!renaming && branch && (
        <span className="ws-row-branch" title={t("ws_branch", { branch })}>
          <GitBranch size={12} strokeWidth={1.5} />
          <span>{branch}</span>
        </span>
      )}
      {!renaming && (
        <span className="ws-row-end">
          {dot && <StatusDot kind={dot} className="ws-row-dot" />}
          {mark && (
            <span className="ws-row-dot ws-row-env" title={item.host}>
              <ComputerMark os={mark.os} online={mark.online} size={13} />
            </span>
          )}
          <button
            type="button"
            className="ws-row-more"
            tabIndex={-1}
            aria-label={t("ws_session_menu", { title })}
            title={t("ws_more")}
            onClick={(e) => onMenu({ anchor: e.currentTarget.getBoundingClientRect() })}
          >
            <MoreHorizontal size={15} strokeWidth={1.5} />
          </button>
        </span>
      )}
    </div>
  );
}

const PENDING_TEXT = { permission: "ws_needs_permission", question: "ws_asked_question", plan: "ws_plan_review" } as const;

/** "5 minutes ago", "yesterday": how long since a moment, in the app's language. */
function since(ms: number): string {
  const rtf = new Intl.RelativeTimeFormat(currentLang() === "zh" ? "zh-CN" : "en", { numeric: "auto" });
  const s = Math.round((ms - Date.now()) / 1000);
  const a = Math.abs(s);
  if (a < 60) return rtf.format(0, "second");
  if (a < 3600) return rtf.format(Math.round(s / 60), "minute");
  if (a < 86400) return rtf.format(Math.round(s / 3600), "hour");
  return rtf.format(Math.round(s / 86400), "day");
}

/** SB-32's card: the whole title, where the session runs, what it is doing or waits for, when it last moved. */
function RowCard({ item, rect }: { item: WsItem; rect: DOMRect }) {
  const box = useRef<HTMLDivElement>(null);
  const [top, setTop] = useState(rect.top);
  useLayoutEffect(() => {
    const h = box.current?.offsetHeight ?? 0;
    setTop(Math.max(8, Math.min(rect.top + rect.height / 2 - h / 2, viewHeight() - h - 8)));
  }, [rect]);
  const m = item.meta;
  const dot = workspace.dot(item);
  const where = [folderName(m.cwd) || m.cwd, item.local ? null : item.host, m.branch?.trim() || null].filter(Boolean).join(" · ");
  const state: { text: string; tone?: "you" | "danger" } | null =
    m.pending && m.state !== "ended"
      ? { text: t(PENDING_TEXT[m.pending]), tone: "you" }
      : dot === "running"
        ? { text: t("ws_card_working") }
        : dot === "failed"
          ? { text: m.last_reason ? `${t("ws_hit_error")}: ${m.last_reason}` : t("ws_hit_error"), tone: "danger" }
          : m.state === "ended"
            ? { text: t("ws_card_ended") }
            : null;
  return (
    <div ref={box} className="ws-hovercard" role="tooltip" style={{ left: rect.right + 8, top }}>
      <div className="ws-hovercard-title">{titleOf(item)}</div>
      <div className="ws-hovercard-where">{where}</div>
      {state && (
        <div className="ws-hovercard-state" data-tone={state.tone}>
          {state.text}
        </div>
      )}
      <div className="ws-hovercard-when">{t("ws_card_active", { when: since(m.last_activity) })}</div>
    </div>
  );
}

/**
 * Renaming in place (SB-22): all selected; Enter or leaving the field keeps it, Esc does not; the
 * trimmed text, empty going back to the automatic title, and nothing when it did not change. A blur
 * right after it opened (250 ms) takes the focus back.
 */
export function RenameField({ initial, onDone, className }: { initial: string; onDone: (title: string | null) => void; className?: string }) {
  const ref = useRef<HTMLInputElement>(null);
  const opened = useRef(Date.now());
  const done = useRef(false);
  const [value, setValue] = useState(initial);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
    if (ref.current) ref.current.scrollLeft = 0;
  }, []);
  const finish = (keep: boolean) => {
    if (done.current) return;
    done.current = true;
    const v = value.trim();
    onDone(keep && v !== initial.trim() ? v : null);
  };
  return (
    <input
      ref={ref}
      className={cx("ws-rename", className)}
      aria-label={t("ws_rename_field")}
      value={value}
      size={Math.min(Math.max(value.length, 1), 30)}
      spellCheck={false}
      onChange={(e) => setValue(e.target.value)}
      onKeyDown={(e) => {
        if (e.nativeEvent.isComposing || e.repeat) return;
        if (e.key === "Enter") {
          e.preventDefault();
          finish(true);
        } else if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          finish(false);
        }
      }}
      onBlur={() => {
        if (Date.now() - opened.current < 250) {
          requestAnimationFrame(() => ref.current?.focus());
          return;
        }
        finish(true);
      }}
    />
  );
}
