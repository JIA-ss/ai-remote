// The workspace's sidebar (clean-room spec SB, flat variant): the 48 px strip with the window corner and
// the computers / agents switch, a search field that opens the ⌘K panel, "New", then the sessions —
// pinned ones first, then "Sessions" with its "Group and sort" menu over one group per folder on a
// computer (or per day, per state, or none; five rows each, "Show N more") — and a footer with this
// device and Settings. Rows are one line: the title fading out at its end, the branch, and the status
// dot that turns into "…" on hover. A right click or "…" opens the row's menu; a group header has its
// own (new session there, copy the path, collapse the others — also Alt+click).

import { ChevronRight, CodeXml, GitBranch, ListFilter, MoreHorizontal, Pin, Plus, Search, Settings } from "lucide-react";
import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import { folderName, knownHome } from "../../lib/chat";
import { currentLang, t, type Key } from "../../lib/i18n";
import type { Computer } from "../../lib/store";
import { EMPTY_SELECTION, readLocked as batchReadLocked, runBatch, selectRow, undoBatch, type BatchChange, type BatchKind, type BatchResult, type RowSelection } from "../../lib/workspace-batch";
import {
  aggregateDot,
  createGroup,
  customSections,
  dateBuckets,
  DEFAULT_DISPLAY,
  DEFAULT_LIST,
  deleteGroup,
  EMPTY_GROUPS,
  descendants,
  familySummary,
  inFamily,
  nestFamilies,
  groupSessions,
  isAscending,
  isDefaultDisplay,
  isDefaultList,
  isFiltered,
  metaLine,
  MOVE_MAX,
  moveGroup,
  moveKey,
  moveToGroup,
  normalizeDisplay,
  normalizeList,
  olderSessions,
  orderBy,
  placeKeys,
  renameGroup,
  restoreGroup,
  undoMove,
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
  type CustomGroup,
  type DayBucket,
  type FamilyNode,
  type Density,
  type DisplayPrefs,
  type GroupBy,
  type ListPrefs,
  type Place,
  type SortMode,
  type StatusFilter,
  type WsItem,
  type WsSnapshot,
} from "../../lib/workspace";
import { cx } from "../util";
import { archive, forkItem, markDone, readLocked, togglePin, toggleRead, unarchive, type ActionCtx } from "./actions";
import { ModeSwitch, type AppMode } from "./ModeSwitch";
import { ComputerMark, modLabel, PopMenu, StatusDot, useOverflow, viewHeight, workspaceOwnsEvent, type MenuAt, type MenuItem, type MenuLine, type Toast } from "./parts";
import { workspace } from "./useWorkspace";

const COLLAPSED = "ai-remote.ws.collapsed";
/** Sessions whose forks are hidden (SB-29), by key. */
const FAMILIES = "ai-remote.ws.families";
/** SB-29: a second click on a fork arrow this soon after the first is ignored. */
const FAMILY_GUARD_MS = 500;
/** How the list groups, sorts and filters (SB-46). */
const LIST = "ai-remote.ws.list";
/** Density and the metadata line (SB-04 / SB-28), kept apart from the list settings. */
const DISPLAY = "ai-remote.ws.display";
const PINNED_KEY = "pinned";
const ALL_KEY = "all";
/** The custom grouping's sections: one per group made here, then the sessions in none. */
const customKey = (id: string) => `custom:${id}`;
const UNGROUPED_KEY = "custom:";
/** A drag starts once the pointer has moved this far (px) from where it went down. */
const DRAG_START = 4;
/** A new group's id: unique on this device. */
const newGroupId = () => `g${Date.now().toString(36)}${Math.random().toString(36).slice(2, 7)}`;

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

function readDisplay(): DisplayPrefs {
  try {
    return normalizeDisplay(JSON.parse(localStorage.getItem(DISPLAY) ?? "null"));
  } catch {
    return DEFAULT_DISPLAY;
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
  // A fork nobody named yet: "Fork of <its parent>".
  if (x.meta.parent && !x.meta.title?.trim()) {
    const parent = workspace.find(x.host, x.meta.parent);
    if (parent && parent.key !== x.key && parent.meta.parent !== x.meta.id) return t("ws_fork_title", { title: titleOf(parent) });
  }
  return workspace.title(x) || t("ws_untitled");
}

function readFamilies(): Set<string> {
  try {
    const v = JSON.parse(localStorage.getItem(FAMILIES) ?? "[]");
    return new Set(Array.isArray(v) ? v.filter((k): k is string => typeof k === "string") : []);
  } catch {
    return new Set();
  }
}

/** One group as the list draws it: a folder on a computer (with its `+`), a day, a state, or everything. */
interface Shown {
  key: string;
  label: string;
  title?: string;
  /** A folder group: where `+` starts a session, and whether the computer is this one. */
  place?: Place & { local: boolean };
  /** A group made here (the custom grouping), or the sessions in none of them. */
  custom?: CustomGroup;
  ungrouped?: boolean;
  /** The date grouping's "Older" bucket, with its bulk action (SB-38). */
  older?: boolean;
  /** The state grouping's "Needs you": its rows need not say they are blocked (SB-28). */
  blocked?: boolean;
  items: WsItem[];
}

/** Where a dragged row or group would land (SB-42). */
type DropTarget =
  | { type: "pin"; before: number }
  | { type: "group"; id: string }
  | { type: "ungroup" }
  | { type: "unpin" }
  | { type: "order"; before: number };

interface DragState {
  kind: "row" | "group";
  /** The rows dragged (the selection when the row is in it), or the group's id. */
  keys: string[];
  /** The rows were all pinned when the drag started. */
  fromPinned: boolean;
  x: number;
  y: number;
  target: DropTarget | null;
  /** The insertion line, in the list's scrolled coordinates. */
  line: number | null;
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
  const [display, setDisplayState] = useState(readDisplay);
  const setDisplay = (change: Partial<DisplayPrefs> | ((d: DisplayPrefs) => DisplayPrefs)) =>
    setDisplayState((prev) => {
      const next = typeof change === "function" ? change(prev) : { ...prev, ...change };
      try {
        localStorage.setItem(DISPLAY, JSON.stringify(next));
      } catch {
        // Kept for this run only.
      }
      return next;
    });
  // A snapshot from before groups existed (a test fixture) has none.
  const groupsState = p.snap.groups ?? EMPTY_GROUPS;
  const { pinned, groups } = useMemo(() => {
    const pinnedOf = (x: WsItem) => workspace.isPinned(x);
    const g = groupSessions(items, { pinned: pinnedOf, title: titleOf, home: knownHome, sort: prefs.sort, reversed: prefs.reversed });
    // SB-42: pins in the order they were dragged into; others follow in the list's order.
    g.pinned = orderBy(g.pinned, groupsState.pinOrder, (x) => x.key);
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
    if (prefs.groupBy === "custom") {
      // SB-08: a section per group made here (empty ones too: something can be dropped there), then the rest.
      const { sections, rest: loose } = customSections(rest, groupsState);
      shown = [
        ...sections.map(({ group, items: xs }) => ({ key: customKey(group.id), label: group.name, custom: group, items: xs })),
        { key: UNGROUPED_KEY, label: t("ws_ungrouped"), ungrouped: true, items: loose },
      ];
    } else if (prefs.groupBy === "date") {
      const by = prefs.sort === "created" ? "started_at" : "last_activity";
      shown = dateBuckets(rest, Date.now(), by, prefs.sort !== "alpha" && isAscending(prefs)).map((b) => ({ key: `date:${b.key}`, label: dayLabel(b), older: b.kind === "older", items: b.items }));
    } else if (prefs.groupBy === "state") {
      shown = stateBuckets(rest, (x) => stateBucket(x.meta, workspace.dot(x))).map((b) => ({ key: `state:${b.key}`, label: t(`ws_state_${b.key}` as Key), blocked: b.key === "blocked", items: b.items }));
    } else {
      shown = rest.length ? [{ key: ALL_KEY, label: t("ws_sessions_head"), items: rest }] : [];
    }
    return { pinned: g.pinned, groups: shown };
  }, [items, p.snap.pins, prefs, groupsState]);
  const [collapsed, setCollapsed] = useState(readCollapsed);
  // SB-29: whose forks are hidden; the arrow ignores a second click within 500 ms.
  const [folds, setFolds] = useState(readFamilies);
  const foldClicks = useRef(new Map<string, number>());
  const toggleFamily = (key: string, guarded = true) => {
    // Only the arrow is guarded against a quick second click; the summary line and the keys are not.
    const now = Date.now();
    if (guarded && now - (foldClicks.current.get(key) ?? 0) < FAMILY_GUARD_MS) return;
    if (guarded) foldClicks.current.set(key, now);
    setFolds((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      try {
        localStorage.setItem(FAMILIES, JSON.stringify([...next].slice(-500)));
      } catch {
        // Kept for this run only.
      }
      return next;
    });
  };
  const detached = useMemo(() => new Set(p.snap.detached), [p.snap.detached]);
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
  // Groups made here (SB-08 / SB-36): "Move to group" for some rows, a header being named or renamed,
  // the confirm steps of deleting a group and of the "Older" bucket's archive (SB-38), and a drag (SB-42).
  const [moveMenu, setMoveMenu] = useState<{ keys: string[]; at: MenuAt } | null>(null);
  const [creating, setCreating] = useState<{ keys: string[] } | null>(null);
  const [renamingGroup, setRenamingGroup] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [olderMenu, setOlderMenu] = useState<{ key: string; at: MenuAt } | null>(null);
  const [confirmOlder, setConfirmOlder] = useState(false);
  const [drag, setDrag] = useState<DragState | null>(null);
  const dragRef = useRef<DragState | null>(null);
  /** When a drag last ended: the click that follows it does not open or fold anything. */
  const dragEnded = useRef(0);
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
      setSelection(EMPTY_SELECTION); setMenu(null); setGroupMenu(null); setListMenu(null); setMoveMenu(null);
    };
    window.addEventListener("keydown", escape, true);
    return () => window.removeEventListener("keydown", escape, true);
  }, [selection.keys.length]);
  // SB-32: a card beside the row the pointer rests on (500 ms; once one shows, the next row's follows at
  // once), gone 200 ms after the pointer leaves; not while a menu is open or a row is being renamed.
  const [card, setCard] = useState<{ key: string; rect: DOMRect } | null>(null);
  const cardTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const quiet = !!menu || !!groupMenu || !!listMenu || !!moveMenu || !!olderMenu || !!drag || !!renamingGroup || !!creating || !!p.renaming;
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
  const ctx: ActionCtx = { toast: p.toast, onArchived: p.onArchived, onOpen: p.onOpen };
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
      {
        key: "batch-move", label: t("ws_move_n_to_group", { n }), disabled: batchBusy || n > MOVE_MAX,
        note: batchBusy ? t("ws_batch_busy") : t("ws_move_too_many", { n: MOVE_MAX }),
        run: () => { const keys = selectedItems.map((x) => x.key); const at = menu?.at; clearSelection(); if (at) setMoveMenu({ keys, at }); },
      },
      "sep", action("archive", !archived, t(archived ? "ws_batch_unarchive" : "ws_batch_archive", { n }), "a"),
    ];
  };

  // ---- Groups made here, pins in their order, and dragging (SB-08 / SB-15 / SB-36 / SB-38 / SB-42) ----
  const pinnedKeys = pinned.map((x) => x.key);
  const failToast = (e: unknown) => p.toast({ text: t("ws_failed", { error: String((e as Error)?.message ?? e) }), tone: "error" });
  // After the change is drawn (a timer, not a frame: a hidden window draws no frames).
  const refocus = (selector: string) => setTimeout(() => list.current?.querySelector<HTMLElement>(selector)?.focus(), 0);
  /** Pins or unpins rows through the batch path (it keeps an old host's pins here); what changed, for an undo. */
  const setPinned = (keys: string[], on: boolean): Promise<BatchChange[]> => {
    const rows = keys.flatMap((k) => {
      const x = byKey.get(k);
      return x && workspace.isPinned(x) !== on ? [{ ...x, meta: { ...x.meta } }] : [];
    });
    if (!rows.length) return Promise.resolve([]);
    return runBatch(workspace, rows, "pin", on).then(
      (r) => {
        if (r.failures.length) p.toast({ text: r.failures.slice(0, 3).map((f) => `${f.title}: ${batchError(f.error)}`).join("; "), tone: "error" });
        return r.successes;
      },
      (e) => {
        failToast(e);
        return [];
      },
    );
  };
  /**
   * SB-36: rows into a group (`null`: out of theirs). A pinned row moved into a group is unpinned, so the
   * move shows; taken out of a group it keeps its pin (`unpin` says otherwise, for a row dragged out of
   * Pinned). The list switches to the custom grouping, where groups show. A toast says it, with an undo.
   */
  const moveRows = (keys: string[], id: string | null, unpin = id !== null) => {
    if (!keys.length) return;
    const { state, moved } = moveToGroup(workspace.get().groups, keys, id);
    const pins = unpin && keys.some((k) => { const x = byKey.get(k); return !!x && workspace.isPinned(x); });
    if (!moved.length && !pins) return;
    workspace.setGroups(state);
    if (id !== null) setPrefs((q) => (q.groupBy === "custom" ? q : { ...q, groupBy: "custom" }));
    const unpinned = pins ? setPinned(keys, false) : Promise.resolve([] as BatchChange[]);
    // Only unpinned (dragged out of Pinned, in no group): the row moving down says it.
    if (!moved.length) return;
    const one = keys.length === 1 ? byKey.get(keys[0]) : undefined;
    const group = state.groups.find((g) => g.id === id)?.name ?? "";
    const text = id !== null
      ? one ? t("ws_moved_one", { title: titleOf(one), group }) : t("ws_moved_many", { n: keys.length, group })
      : one ? t("ws_removed_one", { title: titleOf(one) }) : t("ws_removed_many", { n: keys.length });
    let undone = false;
    p.toast({
      text,
      action: {
        label: t("ws_undo"),
        run: () => {
          if (undone) return;
          undone = true;
          workspace.setGroups(undoMove(workspace.get().groups, moved));
          void unpinned.then((changes) => (changes.length ? undoBatch(workspace, changes) : undefined)).catch(failToast);
        },
      },
    });
  };
  /** Rows pinned before the pinned row at `before` (a drop on Pinned); already pinned ones just move there. */
  const pinRows = (keys: string[], before: number) => {
    workspace.setGroups({ ...workspace.get().groups, pinOrder: placeKeys(pinnedKeys, keys, before) });
    void setPinned(keys, true);
  };
  /** ⌥↑ / ⌥↓ (and the menu): a pinned row one place up or down. */
  const stepPinned = (key: string, by: -1 | 1) => {
    const i = pinnedKeys.indexOf(key);
    if (i < 0 || i + by < 0 || i + by >= pinnedKeys.length) return;
    workspace.setGroups({ ...workspace.get().groups, pinOrder: moveKey(pinnedKeys, key, by < 0 ? i - 1 : i + 2) });
    refocus(`[data-pin-zone] [data-row-key="${CSS.escape(key)}"] .ws-row-main`);
  };
  /** SB-15: a group made here one place up or down (⌥↑ / ⌥↓ on its header, or its menu). */
  const stepGroup = (id: string, by: -1 | 1) => {
    const ids = workspace.get().groups.groups.map((g) => g.id);
    const i = ids.indexOf(id);
    if (i < 0 || i + by < 0 || i + by >= ids.length) return;
    workspace.setGroups(moveGroup(workspace.get().groups, id, by < 0 ? i - 1 : i + 2));
    refocus(`[data-custom-group="${CSS.escape(id)}"] .ws-gh-toggle`);
  };
  /** "New group…": a header to name it in, in the custom grouping; the rows in `keys` then move into it. */
  const startCreate = (keys: string[] = []) => {
    setPrefs((q) => (q.groupBy === "custom" ? q : { ...q, groupBy: "custom" }));
    setMenu(null);
    setMoveMenu(null);
    setGroupMenu(null);
    setListMenu(null);
    setCreating({ keys });
  };
  const finishCreate = (name: string | null) => {
    const keys = creating?.keys ?? [];
    setCreating(null);
    const made = name === null ? null : createGroup(workspace.get().groups, name, newGroupId());
    if (!made) return;
    workspace.setGroups(made.state);
    if (keys.length) moveRows(keys, made.group.id);
    refocus(`[data-custom-group="${CSS.escape(made.group.id)}"] .ws-gh-toggle`);
  };
  const finishRenameGroup = (id: string, name: string | null) => {
    setRenamingGroup(null);
    if (name !== null) workspace.setGroups(renameGroup(workspace.get().groups, id, name));
    refocus(`[data-custom-group="${CSS.escape(id)}"] .ws-gh-toggle`);
  };
  /** SB-08: a group deleted; its sessions go back to Ungrouped. The toast's Undo puts it all back. */
  const removeGroup = (id: string) => {
    const { state, undo } = deleteGroup(workspace.get().groups, id);
    if (!undo) return;
    workspace.setGroups(state);
    let undone = false;
    p.toast({
      text: t("ws_group_deleted", { group: undo.group.name, n: undo.members.length }),
      action: { label: t("ws_undo"), run: () => { if (!undone) { undone = true; workspace.setGroups(restoreGroup(workspace.get().groups, undo)); } } },
    });
  };
  /** SB-36: the groups (digits 1–9 in order), then "Remove from …" when any row is in one, then "New group…". */
  const moveMenuItems = (keys: string[]): MenuLine[] => {
    const lines: MenuLine[] = [];
    let n = 0;
    const digit = () => (++n <= 9 ? String(n) : undefined);
    const tooMany = keys.length > MOVE_MAX;
    const note = tooMany ? t("ws_move_too_many", { n: MOVE_MAX }) : undefined;
    for (const g of groupsState.groups) {
      lines.push({ key: `to-${g.id}`, label: g.name, letter: digit(), checked: keys.every((k) => groupsState.members[k] === g.id), disabled: tooMany, note, run: () => moveRows(keys, g.id) });
    }
    const inGroups = [...new Set(keys.map((k) => groupsState.members[k]).filter((id): id is string => !!id && groupsState.groups.some((g) => g.id === id)))];
    if (inGroups.length) {
      const only = inGroups.length === 1 && keys.every((k) => groupsState.members[k] === inGroups[0]) ? groupsState.groups.find((g) => g.id === inGroups[0])?.name : undefined;
      lines.push("sep", { key: "out", label: only ? t("ws_remove_from", { group: only }) : t("ws_remove_from_groups"), letter: digit(), run: () => moveRows(keys, null, false) });
    }
    if (lines.length) lines.push("sep");
    lines.push({ key: "new-group", label: t("ws_new_group"), letter: digit(), disabled: tooMany, note, run: () => startCreate(keys) });
    return lines;
  };
  /** SB-38: the "Older" bucket's bulk archive — sessions a week or more old, confirmed by a second click. */
  const olderItems = (key: string): MenuLine[] => {
    const g = groups.find((x) => x.key === key);
    const old = g ? olderSessions(g.items, Date.now()) : [];
    if (!old.length) return [{ key: "older-none", label: t("ws_older_none"), disabled: true, run: () => undefined }];
    return [{
      key: "older-archive",
      label: confirmOlder ? t("ws_older_archive_confirm", { n: old.length }) : t("ws_older_archive", { n: old.length }),
      danger: true,
      keep: !confirmOlder,
      disabled: batchBusy,
      note: t("ws_batch_busy"),
      run: () => {
        if (!confirmOlder) { setConfirmOlder(true); return; }
        setConfirmOlder(false);
        const chosen = old.map((x) => ({ ...x, meta: { ...x.meta } }));
        dispatchBatch(() => runBatch(workspace, chosen, "archive", true), t("ws_older_label"), true);
      },
    }];
  };

  /** SB-42: what a drag would land on at a point — Pinned (with its insertion line), a group, Ungrouped, out of Pinned, or a group's new place. */
  const dropAt = (d: DragState, x: number, y: number): Pick<DragState, "target" | "line"> => {
    const box = list.current;
    const none = { target: null, line: null };
    if (!box) return none;
    const r = box.getBoundingClientRect();
    if (x < r.left || x > r.right || y < r.top || y > r.bottom) return none;
    const at = (el: Element, edge: "top" | "bottom") => el.getBoundingClientRect()[edge] - r.top + box.scrollTop;
    const lineBefore = (els: Element[], before: number) => (before < els.length ? at(els[before], "top") - 1 : at(els[els.length - 1], "bottom") + 1);
    const beforeOf = (els: Element[]) => {
      const i = els.findIndex((el) => { const b = el.getBoundingClientRect(); return y < b.top + b.height / 2; });
      return i < 0 ? els.length : i;
    };
    if (d.kind === "group") {
      const secs = [...box.querySelectorAll("[data-custom-group]")];
      if (!secs.length) return none;
      const before = beforeOf(secs);
      return { target: { type: "order", before }, line: lineBefore(secs, before) };
    }
    const el = document.elementFromPoint(x, y);
    const zone = el?.closest("[data-pin-zone]");
    if (zone && box.contains(zone)) {
      const rows = [...zone.querySelectorAll(".ws-row[data-row-key]")];
      if (el?.closest("[data-drop=pin]") || !rows.length) return { target: { type: "pin", before: 0 }, line: null };
      const before = beforeOf(rows);
      return { target: { type: "pin", before }, line: lineBefore(rows, before) };
    }
    const custom = el?.closest<HTMLElement>("[data-custom-group]");
    if (custom?.dataset.customGroup && box.contains(custom)) return { target: { type: "group", id: custom.dataset.customGroup }, line: null };
    if (el?.closest("[data-ungrouped]")) return { target: { type: "ungroup" }, line: null };
    if (d.fromPinned && el && box.contains(el)) return { target: { type: "unpin" }, line: null };
    return none;
  };
  const drop = (d: DragState) => {
    const target = d.target;
    if (!target) return;
    if (d.kind === "group") {
      if (target.type === "order") workspace.setGroups(moveGroup(workspace.get().groups, d.keys[0], target.before));
      return;
    }
    if (target.type === "pin") pinRows(d.keys, target.before);
    else if (target.type === "group") moveRows(d.keys, target.id);
    else if (target.type === "ungroup") moveRows(d.keys, null, d.fromPinned);
    else if (target.type === "unpin") void setPinned(d.keys, false);
  };
  /**
   * A row (or a group's header) pressed with the mouse or a pen: past 4 px it is dragged. The selection
   * goes along when the row is in it. Esc, losing the window or a cancelled pointer drop nothing; the
   * click after a drag is swallowed.
   */
  const startDrag = (e: React.PointerEvent, kind: DragState["kind"], key: string) => {
    if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey || e.pointerType === "touch" || p.renaming || renamingGroup || creating) return;
    const x0 = e.clientX;
    const y0 = e.clientY;
    let started = false;
    const stop = () => {
      window.removeEventListener("pointermove", move, true);
      window.removeEventListener("pointerup", up, true);
      window.removeEventListener("pointercancel", cancel, true);
      window.removeEventListener("keydown", escape, true);
      window.removeEventListener("blur", cancel);
      if (started) dragEnded.current = Date.now();
      dragRef.current = null;
      setDrag(null);
    };
    const move = (ev: PointerEvent) => {
      if (!started) {
        if (Math.hypot(ev.clientX - x0, ev.clientY - y0) < DRAG_START) return;
        started = true;
        const keys = kind === "row" && selection.keys.length > 1 && selection.keys.includes(key) ? selection.keys.filter((k) => byKey.has(k)) : [key];
        const fromPinned = kind === "row" && keys.every((k) => { const x = byKey.get(k); return !!x && workspace.isPinned(x); });
        dragRef.current = { kind, keys, fromPinned, x: ev.clientX, y: ev.clientY, target: null, line: null };
        setMenu(null);
        setGroupMenu(null);
        setListMenu(null);
        setMoveMenu(null);
        setOlderMenu(null);
      }
      ev.preventDefault();
      const d = dragRef.current;
      if (!d) return;
      const box = list.current;
      if (box) {
        const r = box.getBoundingClientRect();
        if (ev.clientY < r.top + 24) box.scrollTop -= 8;
        else if (ev.clientY > r.bottom - 24) box.scrollTop += 8;
      }
      dragRef.current = { ...d, x: ev.clientX, y: ev.clientY, ...dropAt(d, ev.clientX, ev.clientY) };
      setDrag(dragRef.current);
    };
    const up = (ev: PointerEvent) => {
      const d = dragRef.current;
      stop();
      if (d) drop({ ...d, ...dropAt(d, ev.clientX, ev.clientY) });
    };
    const cancel = () => stop();
    const escape = (ev: globalThis.KeyboardEvent) => {
      if (ev.key !== "Escape" || !started) return;
      ev.preventDefault();
      ev.stopPropagation();
      stop();
    };
    window.addEventListener("pointermove", move, true);
    window.addEventListener("pointerup", up, true);
    window.addEventListener("pointercancel", cancel, true);
    window.addEventListener("keydown", escape, true);
    window.addEventListener("blur", cancel);
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
    // SB-42 / SB-15 by keyboard: ⌥↑ / ⌥↓ move a pinned row within Pinned, or a group made here among the groups.
    if (e.altKey && !e.metaKey && !e.ctrlKey && !e.shiftKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
      const by = e.key === "ArrowUp" ? -1 : 1;
      const el = document.activeElement as HTMLElement | null;
      const custom = el?.matches(".ws-gh-toggle") ? el.closest<HTMLElement>("[data-custom-group]")?.dataset.customGroup : undefined;
      if (from && pinnedKeys.includes(from) && el?.closest("[data-pin-zone]")) {
        e.preventDefault();
        stepPinned(from, by);
      } else if (custom) {
        e.preventDefault();
        stepGroup(custom, by);
      }
      return;
    }
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
    for (const x of items) ids.add(`row:${x.key}`).add(`sum:${x.key}`);
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
    // SB-35's "organize": order within Pinned, and Move to group (SB-36).
    const pinAt = pinnedKeys.indexOf(x.key);
    const organize: MenuLine[] = [
      ...(pinAt >= 0
        ? [
            { key: "pin-up", label: t("ws_pinned_up"), disabled: pinAt === 0, run: () => stepPinned(x.key, -1) },
            { key: "pin-down", label: t("ws_pinned_down"), disabled: pinAt === pinnedKeys.length - 1, run: () => stepPinned(x.key, 1) },
          ]
        : []),
      { key: "move", label: t("ws_move_to_group"), run: () => { const at = menu?.at; if (at) setMoveMenu({ keys: [x.key], at }); } },
    ];
    return [
      { key: "pin", label: t(workspace.isPinned(x) ? "ws_unpin" : "ws_pin"), letter: "p", run: () => togglePin(x, ctx) },
      read,
      { key: "rename", label: t("ws_rename"), letter: "r", disabled: old, note: oldNote, run: () => p.setRenaming(x.key) },
      forkItem(x, ctx),
      "sep",
      ...organize,
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
    // A group made here (SB-45): rename in place, its place among the groups (SB-15), a new one; delete at the end.
    const custom = g?.custom;
    const order = groupsState.groups.findIndex((x) => x.id === custom?.id);
    if (custom) {
      lines.push(
        { key: "rename-group", label: t("ws_group_rename"), letter: "r", run: () => setRenamingGroup(custom.id) },
        { key: "group-up", label: t("ws_group_up"), disabled: order <= 0, run: () => stepGroup(custom.id, -1) },
        { key: "group-down", label: t("ws_group_down"), disabled: order >= groupsState.groups.length - 1, run: () => stepGroup(custom.id, 1) },
        "sep",
      );
    }
    if (custom || g?.ungrouped) lines.push({ key: "new-group", label: t("ws_new_group"), run: () => startCreate() }, "sep");
    const keys = [...(pinned.length ? [PINNED_KEY] : []), ...groups.map((x) => x.key)];
    lines.push(
      { key: "fold", label: t("ws_fold_others"), disabled: keys.every((k) => k === key || collapsed.has(k)), run: () => toggleGroup(key, true) },
      { key: "unfold", label: t("ws_expand_all"), disabled: !keys.some((k) => collapsed.has(k)), run: () => saveCollapsed(new Set()) },
    );
    if (g?.older) lines.push("sep", ...olderItems(g.key));
    if (custom) {
      lines.push("sep", {
        key: "delete-group",
        label: confirmDelete === custom.id ? t("ws_group_delete_confirm") : t("ws_group_delete"),
        letter: "d",
        danger: true,
        keep: confirmDelete !== custom.id,
        run: () => {
          if (confirmDelete !== custom.id) setConfirmDelete(custom.id);
          else {
            setConfirmDelete(null);
            removeGroup(custom.id);
          }
        },
      });
    }
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
    const density = (v: Density, k: Key) => choice(`d-${v}`, t(k), display.density === v, () => setDisplay({ density: v }));
    return [
      { key: "select-all", label: t("ws_select_all"), disabled: !!selectAllNote, note: selectAllNote, run: selectAll },
      { key: "new-group", label: t("ws_new_group"), run: () => startCreate() },
      "sep",
      { heading: t("ws_group_by") },
      groupBy("project", "ws_group_project"),
      groupBy("custom", "ws_group_custom"),
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
      // SB-04 / SB-28: how rows are drawn.
      { heading: t("ws_rows_head") },
      density("compact", "ws_density_compact"),
      density("comfortable", "ws_density_comfortable"),
      { ...choice("meta", t("ws_show_meta"), display.meta, () => setDisplay((d) => ({ ...d, meta: !d.meta }))), detail: t("ws_show_meta_detail") },
      "sep",
      {
        key: "reset",
        label: t("ws_list_reset"),
        disabled: isDefaultList(prefs) && isDefaultDisplay(display),
        note: t("ws_list_reset_none"),
        run: () => {
          setPrefs(() => DEFAULT_LIST);
          setDisplay(() => DEFAULT_DISPLAY);
        },
      },
    ];
  };

  const row = (x: WsItem, opts: { lead?: boolean; env?: boolean; groupFolder?: string; blocked?: boolean; depth?: number; family?: { open: boolean; onToggle: (guarded?: boolean) => void } } = {}) => (
    <Row
      key={x.key}
      item={x}
      lead={opts.lead}
      depth={opts.depth ?? 0}
      family={opts.family}
      metaOn={display.meta}
      meta={display.meta ? metaLine(x.meta, { groupFolder: opts.groupFolder, underBlocked: opts.blocked, host: !x.local && prefs.groupBy !== "project" ? x.host : null }) : null}
      dragging={!!drag && drag.kind === "row" && drag.keys.includes(x.key)}
      onDragStart={(e) => startDrag(e, "row", x.key)}
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

  // SB-29: a session, then (while not hidden) its forks indented under it with the guide line; hidden
  // forks leave one line saying how many there are, and how many failed, are unread or run.
  const family = (n: FamilyNode<WsItem>, depth: number, opts: { env?: boolean; groupFolder?: string; blocked?: boolean }): ReactNode => {
    const key = n.item.key;
    if (!n.children.length) return row(n.item, { ...opts, depth });
    const open = !folds.has(key);
    const kids = open ? n.children : n.children.filter((c) => inFamily(c, p.current));
    // The line counts the forks it stands for: those shown (the open one) are not hidden.
    const hidden = n.children.filter((c) => !kids.includes(c));
    const sum = open || !hidden.length ? null : familySummary({ item: n.item, children: hidden }, (x) => workspace.dot(x));
    const depthStyle = { "--ws-depth": depth } as CSSProperties;
    return (
      <Fragment key={key}>
        {row(n.item, { ...opts, depth, family: { open, onToggle: (guarded = true) => toggleFamily(key, guarded) } })}
        <div className="ws-family" style={depthStyle} data-family-of={key}>
          {kids.map((c) => family(c, depth + 1, opts))}
          {sum && (
            <button
              type="button"
              className="ws-family-sum"
              style={{ "--ws-depth": depth } as CSSProperties}
              data-ws-nav
              tabIndex={navIndex(`sum:${key}`)}
              onFocus={() => setFocusId(`sum:${key}`)}
              aria-expanded={false}
              onClick={() => toggleFamily(key, false)}
            >
              <span>{t(sum.count === 1 ? "ws_family_count_one" : "ws_family_count_many", { n: sum.count })}</span>
              {sum.failed > 0 && <span className="bad">· {t("ws_family_failed", { n: sum.failed })}</span>}
              {sum.unread > 0 && <span>· {t("ws_family_unread", { n: sum.unread })}</span>}
              {sum.running > 0 && <span>· {t("ws_family_running", { n: sum.running })}</span>}
            </button>
          )}
        </div>
      </Fragment>
    );
  };

  const noComputers = p.computers.length === 0;
  const loading = !all.length && p.computers.some((c) => c.online === true && (!p.snap.hosts[c.host.name] || p.snap.hosts[c.host.name].status === "loading"));
  const notes = p.computers
    .map((c) => ({ c, h: p.snap.hosts[c.host.name] }))
    .filter(({ c, h }) => c.online === true && h && (h.status === "denied" || h.status === "unsupported" || (h.status === "error" && !h.sessions.length)));
  const filtered = isFiltered(prefs);
  // SB-47: off the folder grouping (where the header names the computer), rows from other computers carry its mark.
  const envRows = prefs.groupBy !== "project";
  const listLabel = filtered ? t("ws_group_sort_on") : t("ws_group_sort");
  // While a row that is not pinned is dragged, Pinned shows its drop row (and appears when empty).
  const pinDrop = !!drag && drag.kind === "row" && !drag.fromPinned;
  const overPinRow = drag?.target?.type === "pin" && drag.line === null;
  const dragTitle = drag ? (drag.kind === "group" ? (groupsState.groups.find((g) => g.id === drag.keys[0])?.name ?? "") : drag.keys.length > 1 ? t("ws_drag_count", { n: drag.keys.length }) : byKey.get(drag.keys[0]) ? titleOf(byKey.get(drag.keys[0]) as WsItem) : "") : "";
  const dragHint = !drag?.target
    ? null
    : drag.target.type === "pin" ? t("ws_drop_pin_over")
      : drag.target.type === "unpin" ? t("ws_drop_unpin")
        : drag.target.type === "ungroup" ? t("ws_ungrouped")
          : drag.target.type === "group" ? (groupsState.groups.find((g) => g.id === (drag.target as { id: string }).id)?.name ?? null)
            : null;
  const creatingHeader = creating && prefs.groupBy === "custom" && (
    <div className="ws-sec ws-sec-new" data-group="custom:new">
      <div className="ws-gh">
        <RenameField initial="" label={t("ws_group_name_field")} className="ws-gh-rename" onDone={finishCreate} />
      </div>
    </div>
  );

  return (
    <nav ref={side} className="ws-side" aria-label={t("ws_sidebar")} data-density={display.density} data-dragging={drag ? drag.kind : undefined}>
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
      <div
        className="ws-list"
        ref={list}
        onKeyDown={onListKey}
        onScroll={() => card && setCard(null)}
        onClickCapture={(e) => {
          // The click that ends a drag neither opens the row nor folds the group it was let go on.
          if (Date.now() - dragEnded.current < 120) {
            e.preventDefault();
            e.stopPropagation();
          }
        }}
        role="list"
        aria-label={t("ws_sessions")}
      >
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
        {(pinned.length > 0 || pinDrop) && (
          <Section
            id={PINNED_KEY}
            label={t("ws_pinned")}
            collapsed={collapsed.has(PINNED_KEY)}
            onToggle={(others) => toggleGroup(PINNED_KEY, others)}
            onMenu={(at) => setGroupMenu({ key: PINNED_KEY, at })}
            dot={aggregateDot(pinned.map((x) => workspace.dot(x)))}
            tabIndex={navIndex(`group:${PINNED_KEY}`)}
            onFocus={() => setFocusId(`group:${PINNED_KEY}`)}
            attrs={{ "data-pin-zone": "" }}
            // SB-42: while a row that is not pinned is dragged, Pinned's header line becomes the row it is
            // let go on to pin it (the same height, so nothing under the pointer moves).
            head={
              pinDrop ? (
                <div className="ws-pin-drop" data-drop="pin" data-over={overPinRow ? "" : undefined}>
                  <Pin className="ws-pin-drop-icon" size={14} strokeWidth={1.5} />
                  <span>{overPinRow ? t("ws_drop_pin_over") : t("ws_drop_pin")}</span>
                </div>
              ) : undefined
            }
          >
            {(collapsed.has(PINNED_KEY) ? pinned.filter((x) => x.key === p.current) : pinned).map((x) => row(x, { lead: true }))}
          </Section>
        )}
        {/* With nothing pinned, Pinned's drop row takes the place of this header line during a drag. */}
        {(all.length > 0 || filtered) && !noComputers && !(pinDrop && !pinned.length) && (
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
        {filtered && !groups.some((g) => g.items.length) && (
          <div className="ws-filter-empty">
            <p>{t("ws_no_match")}</p>
            <button type="button" className="ws-link" onClick={() => setPrefs({ status: "active" })}>
              {t("ws_show_all")}
            </button>
          </div>
        )}
        {groups.map((g) => {
          // Ungrouped shows when it has rows, or as a place to drop a row out of its group (or out of Pinned).
          if (g.ungrouped && !g.items.length && drag?.kind !== "row") return creatingHeader ? <Fragment key="custom:new">{creatingHeader}</Fragment> : null;
          const folded = collapsed.has(g.key);
          const keep = new Set(p.current ? [p.current] : []);
          const limit = g.ungrouped ? rowsBeforeMore("none") : rowsBeforeMore(prefs.groupBy);
          // Families are cut as one: a fork shows with the session it came from.
          const nodes = nestFamilies(g.items, detached);
          const byRoot = new Map(nodes.map((n) => [n.item.key, n]));
          for (const n of nodes) if (inFamily(n, p.current)) keep.add(n.item.key);
          const cut = truncateGroup(nodes.map((n) => n.item), limit, keep, opened.has(g.key), (x) => {
            const n = byRoot.get(x.key);
            return !!n && [n.item, ...descendants(n)].some((y) => workspace.dot(y) === "awaiting");
          });
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
                  {rows.map((x) => (byRoot.has(x.key) ? family(byRoot.get(x.key)!, 0, { env: envRows }) : row(x, { env: envRows })))}
                  {more}
                </div>
              </div>
            );
          }
          const custom = g.custom;
          const section = (
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
              attrs={custom ? { "data-custom-group": custom.id } : g.ungrouped ? { "data-ungrouped": "" } : undefined}
              dropOver={!!drag?.target && ((drag.target.type === "group" && drag.target.id === custom?.id) || (drag.target.type === "ungroup" && !!g.ungrouped))}
              dragging={drag?.kind === "group" && drag.keys[0] === custom?.id}
              renaming={!!custom && renamingGroup === custom.id}
              onRenamed={custom ? (name) => finishRenameGroup(custom.id, name) : undefined}
              onHeadPointerDown={custom ? (e) => startDrag(e, "group", custom.id) : undefined}
              trailing={
                g.older ? (
                  <button
                    type="button"
                    className="ws-gh-add"
                    tabIndex={-1}
                    aria-label={t("ws_older_bulk")}
                    title={t("ws_older_bulk")}
                    aria-expanded={olderMenu?.key === g.key}
                    onClick={(e) => {
                      setConfirmOlder(false);
                      setOlderMenu(olderMenu ? null : { key: g.key, at: { anchor: e.currentTarget.getBoundingClientRect() } });
                    }}
                  >
                    <MoreHorizontal size={15} strokeWidth={1.5} />
                  </button>
                ) : place && (
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
              {(() => {
                const o = { env: envRows, groupFolder: place?.cwd, blocked: g.blocked };
                return folded ? rows.map((x) => row(x, o)) : rows.map((x) => (byRoot.has(x.key) ? family(byRoot.get(x.key)!, 0, o) : row(x, o)));
              })()}
              {more}
              {!g.items.length && !folded && (
                <div className="ws-group-empty">{custom ? t("ws_group_empty") : t("ws_drop_ungroup")}</div>
              )}
            </Section>
          );
          return g.ungrouped && creatingHeader ? (
            <Fragment key={g.key}>
              {creatingHeader}
              {section}
            </Fragment>
          ) : (
            section
          );
        })}
        {drag?.line != null && <div className="ws-drop-line" style={{ top: drag.line }} aria-hidden />}
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
      {moveMenu && <PopMenu at={moveMenu.at} className="ws-move-menu" label={t("ws_move_menu")} items={moveMenuItems(moveMenu.keys)} onClose={() => setMoveMenu(null)} />}
      {olderMenu && <PopMenu at={olderMenu.at} className="ws-older-menu" label={t("ws_older_bulk")} items={olderItems(olderMenu.key)} onClose={() => { setOlderMenu(null); setConfirmOlder(false); }} />}
      {card && byKey.get(card.key) && <RowCard item={byKey.get(card.key) as WsItem} rect={card.rect} />}
      {drag && (
        <div className="ws-drag-ghost" style={{ left: drag.x + 12, top: drag.y + 10 }} aria-hidden>
          <span className="ws-drag-ghost-title">{dragTitle}</span>
          {dragHint && <span className="ws-drag-ghost-hint">{dragHint}</span>}
        </div>
      )}
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
  attrs,
  dropOver,
  dragging,
  renaming,
  onRenamed,
  onHeadPointerDown,
  head,
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
  /** What the drag code looks for: `data-pin-zone`, `data-custom-group`, `data-ungrouped`. */
  attrs?: Record<string, string>;
  /** A dragged row would land here (SB-42's 2 px ring). */
  dropOver?: boolean;
  /** This group's header is the one being dragged. */
  dragging?: boolean;
  /** A group made here being renamed in place (SB-45): Enter or leaving keeps it, Esc does not. */
  renaming?: boolean;
  onRenamed?: (name: string | null) => void;
  onHeadPointerDown?: (e: React.PointerEvent) => void;
  /** What the header line shows instead of its label (Pinned's drop row during a drag). */
  head?: ReactNode;
  children: ReactNode;
}) {
  const ref = useOverflow<HTMLSpanElement>(label);
  // VS-34: opening a group grows its rows from nothing (grid rows 0fr → 1fr) while they fade in, 150 ms
  // ease-out; folding is at once. The attribute goes when the animation ends (or after 260 ms: reduced
  // motion has no animation to end).
  const wasCollapsed = useRef(collapsed);
  const [expanding, setExpanding] = useState(false);
  useLayoutEffect(() => {
    const opened = wasCollapsed.current && !collapsed;
    wasCollapsed.current = collapsed;
    if (!opened) {
      setExpanding(false);
      return;
    }
    setExpanding(true);
    const id = setTimeout(() => setExpanding(false), 260);
    return () => clearTimeout(id);
  }, [collapsed]);
  return (
    <div className="ws-sec" data-collapsed={collapsed ? "" : undefined} data-group={id} data-drop-over={dropOver ? "" : undefined} data-dragging={dragging ? "" : undefined} {...attrs}>
      <div
        className="ws-gh"
        onContextMenu={(e) => {
          e.preventDefault();
          onMenu({ x: e.clientX, y: e.clientY });
        }}
      >
        {head ? (
          head
        ) : renaming && onRenamed ? (
          <RenameField initial={label} label={t("ws_group_name_field")} className="ws-gh-rename" onDone={onRenamed} />
        ) : (
          <button
            type="button"
            className="ws-gh-toggle"
            data-ws-nav
            data-group-toggle
            aria-expanded={!collapsed}
            title={title}
            tabIndex={tabIndex}
            onFocus={onFocus}
            onPointerDown={onHeadPointerDown}
            onClick={(e) => onToggle(e.altKey)}
          >
            <span className="ws-gh-label" ref={ref}>
              {label}
            </span>
            <ChevronRight className="ws-gh-caret" size={12} strokeWidth={2} />
          </button>
        )}
        {!head && <span className="ws-gh-badge">{collapsed && dot && <StatusDot kind={dot} />}</span>}
        {!head && trailing}
      </div>
      <div
        className="ws-sec-body"
        data-expanding={expanding ? "" : undefined}
        onAnimationEnd={(e) => {
          if (e.target === e.currentTarget) setExpanding(false);
        }}
      >
        <div className="ws-sec-rows" role="group" aria-label={label}>
          {children}
        </div>
      </div>
    </div>
  );
}

function Row({
  item,
  lead,
  depth,
  family,
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
  metaOn,
  meta,
  dragging,
  onDragStart,
}: {
  item: WsItem;
  lead?: boolean;
  /** How far under the session it was forked from (SB-29). */
  depth: number;
  /** A session with forks: whether they show, and the arrow that shows or hides them. */
  family?: { open: boolean; onToggle: (guarded?: boolean) => void };
  jump?: number;
  /** SB-28: the metadata line is on (the branch moves there), and what it says (null: nothing for this row). */
  metaOn: boolean;
  meta: ReturnType<typeof metaLine>;
  /** The row is part of what is being dragged (SB-42). */
  dragging: boolean;
  onDragStart: (e: React.PointerEvent) => void;
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
  const branch = metaOn ? undefined : item.meta.branch?.trim();
  const mark = !dot && env ? env : null;
  const decorated = !!dot || !!branch || !!mark;
  const metaParts = meta ? (meta.blocked ? [t("ws_blocked"), t(PENDING_TEXT[meta.blocked])] : [meta.branch, meta.folder, meta.host].filter((x): x is string => !!x)) : [];
  const metaText = metaParts.length ? new Intl.ListFormat(currentLang() === "zh" ? "zh-CN" : "en", { type: "unit" }).format(metaParts) : "";
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
      data-family={family ? (family.open ? "open" : "closed") : undefined}
      data-depth={depth || undefined}
      style={depth ? ({ "--ws-depth": depth } as CSSProperties) : undefined}
      onKeyDown={(e) => {
        // A tree's keys: → shows the forks, ← hides them.
        if (!family || e.altKey || e.metaKey || e.ctrlKey || e.shiftKey) return;
        if ((e.key === "ArrowRight" && !family.open) || (e.key === "ArrowLeft" && family.open)) {
          e.preventDefault();
          family.onToggle(false);
        }
      }}
      data-meta={metaOn && meta ? "" : undefined}
      data-dragging={dragging ? "" : undefined}
      onPointerEnter={(e) => e.pointerType === "mouse" && onHover(e.currentTarget)}
      onPointerLeave={() => onHover(null)}
      onPointerDown={(e) => {
        onHover(null);
        if (!renaming && !(e.target as Element).closest(".ws-row-more")) onDragStart(e);
      }}
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
          onClick={(e) => {
            // The arrow after the title (inside the row's button) shows or hides the forks.
            if (family && (e.target as HTMLElement).closest(".ws-row-family")) {
              e.preventDefault();
              family.onToggle();
              return;
            }
            onOpen(e);
          }}
          aria-expanded={family ? family.open : undefined}
          aria-current={selected ? "page" : undefined}
          aria-pressed={multiSelected}
          aria-description={item.meta.archived ? t("ws_archived_row") : undefined}
        >
          {(lead || jump) && (
            <span className="ws-row-lead">
              {jump ? <kbd className="ws-jump-key" aria-hidden>{jump}</kbd> : <CodeXml size={14} strokeWidth={1.5} />}
            </span>
          )}
          <span className="ws-row-text">
            <span className="ws-row-title" ref={ref}>
              {title}
            </span>
            {metaOn && meta && (
              <span className="ws-row-meta" title={metaText}>
                {meta.blocked ? (
                  <>
                    <span className="ws-row-blocked">{t("ws_blocked")}</span>
                    <span className="ws-row-meta-part">{t(PENDING_TEXT[meta.blocked])}</span>
                  </>
                ) : (
                  <>
                    {meta.branch && (
                      <span className="ws-row-meta-part">
                        <GitBranch size={12} strokeWidth={1.5} />
                        {meta.branch}
                      </span>
                    )}
                    {meta.folder && <span className="ws-row-meta-part">{meta.folder}</span>}
                    {meta.host && <span className="ws-row-meta-part">{meta.host}</span>}
                  </>
                )}
              </span>
            )}
          </span>
          {family && (
            <span className="ws-row-family" data-open={family.open ? "" : undefined} title={t("ws_family_toggle")} aria-hidden>
              <ChevronRight size={13} strokeWidth={1.75} />
            </span>
          )}
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
export function RenameField({ initial, onDone, className, label }: { initial: string; onDone: (title: string | null) => void; className?: string; label?: string }) {
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
      aria-label={label ?? t("ws_rename_field")}
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
