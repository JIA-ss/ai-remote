// The agents workspace (docs/agent-workspace.zh-CN.md §2.2): its own sidebar beside a page — a new
// session, or one session — filling the window. The sidebar is 288 px by default, 200–420 by dragging
// its edge (a click on the edge or ⌘B hides it, a double click resets it), and folds away in a narrow
// window, where ⌘B or the toggle lays it over the page instead (SB-02 / SB-03). ⌘K searches, ⌘N starts
// a new session (⌘⇧N where the open one runs), F2 renames, ⌘⌥P / R / U / A pin, rename, change the read
// state and archive (SB-39). Back / forward are the shell's history (lib/nav.ts): the workspace is one
// route of the window, every page it opens is an entry there, so ‹ › and ⌘[ ⌘] walk across the
// workspace, the live views it opens and the computers alike.

import { useCallback, useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";
import { t } from "../../lib/i18n";
import type { Computer } from "../../lib/store";
import { clampSidebar, narrowAt, sessionKey, SIDEBAR, sidebarMax, type Place, type WsItem } from "../../lib/workspace";
import { cx } from "../util";
import type { ChatTarget } from "../AgentChat";
import type { AppMode } from "./ModeSwitch";
import { NewSession } from "./NewSession";
import { Palette } from "./Palette";
import { Corner, modKey, Toasts, viewWidth, workspaceOwnsEvent, type Toast } from "./parts";
import { SessionPage } from "./SessionPage";
import { Sidebar } from "./Sidebar";
import { archive, fork, markDone, togglePin, toggleRead, unarchive } from "./actions";
import { FamilyConfirm } from "./FamilyConfirm";
import { useWorkspace, workspace } from "./useWorkspace";

/** The session shortcuts' keys (with ⌘⌥ / Ctrl+Alt). */
const SESSION_KEYS = new Set(["KeyP", "KeyR", "KeyU", "KeyA", "KeyO"]);

/** Where the workspace is: a session on a computer, or the new-session page (with the place it was
 *  opened for). The shell's `{ page: "agents" }` route carries the same fields. */
export interface WsRoute {
  host?: string;
  session?: string;
  place?: Place | null;
}

const SIDE = "ai-remote.ws.side";

function readSide(): { w: number; hidden: boolean } {
  try {
    const v = JSON.parse(localStorage.getItem(SIDE) ?? "{}") as { w?: number; hidden?: boolean };
    return { w: typeof v.w === "number" ? v.w : SIDEBAR.DEFAULT, hidden: !!v.hidden };
  } catch {
    return { w: SIDEBAR.DEFAULT, hidden: false };
  }
}

function useWorkspaceWidth(root: RefObject<HTMLDivElement | null>, embedded: boolean) {
  const [w, setW] = useState(viewWidth());
  useLayoutEffect(() => {
    const on = () => setW(embedded ? root.current?.clientWidth ?? viewWidth() : viewWidth());
    on();
    window.addEventListener("resize", on);
    const ro = embedded && root.current ? new ResizeObserver(on) : null;
    if (ro && root.current) ro.observe(root.current);
    return () => {
      window.removeEventListener("resize", on);
      ro?.disconnect();
    };
  }, [embedded, root]);
  return w;
}

const editable = (el: EventTarget | null) => el instanceof HTMLElement && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName));

export function Workspace({
  computers,
  device,
  route,
  onRoute,
  back,
  forward,
  onMode,
  onWatch,
  onPair,
  onSettings,
  embedded = false,
  onPlaceChange,
  initialTarget,
}: {
  computers: Computer[];
  device: string;
  route: WsRoute;
  /** Opens a workspace page: a new entry in the shell's history. */
  onRoute: (r: WsRoute) => void;
  /** The shell's history; null when there is nothing that way. */
  back: (() => void) | null;
  forward: (() => void) | null;
  onMode: (m: AppMode) => void;
  onWatch: (host: string, session?: string) => void;
  onPair: () => void;
  onSettings: () => void;
  /** The complete workspace beside a live view, sized and keyboard-scoped to its own pane. */
  embedded?: boolean;
  /** A local new-session computer / folder choice; lets the live view follow the same computer. */
  onPlaceChange?: (place: Place) => void;
  /** A first-use chat_start ticket supplied by the page that opened this workspace. */
  initialTarget?: (ChatTarget & { host: string }) | null;
}) {
  const snap = useWorkspace(computers, true);
  const [side, setSide] = useState(readSide);
  const [floatOpen, setFloatOpen] = useState(false);
  /** The folded sidebar was opened by the pointer at the window's left edge (it goes when the pointer leaves). */
  const [peek, setPeek] = useState(false);
  const [edgeHint, setEdgeHint] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const [dragging, setDragging] = useState(false);
  const winW = useWorkspaceWidth(root, embedded);
  const narrow = winW <= narrowAt(side.w);
  const docked = !narrow && !side.hidden;
  const shown = docked || (narrow && floatOpen);
  const width = Math.min(side.w, sidebarMax(winW));
  const [palette, setPalette] = useState(false);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [renamingRow, setRenamingRow] = useState<string | null>(null);
  const [renamingTitle, setRenamingTitle] = useState(false);
  /** Tickets from `chat_start`, for the first look at a session just started here. */
  const tickets = useRef(new Map<string, string>());
  const current = route.host && route.session ? sessionKey(route.host, route.session) : null;

  useLayoutEffect(() => {
    const pane = root.current;
    if (!embedded || !pane) return;
    const own = (target: EventTarget | null) => target instanceof Node && pane.contains(target);
    const set = (target: EventTarget | null) => { pane.dataset.keyboardActive = own(target) ? "true" : "false"; };
    set(document.activeElement);
    const pointer = (e: PointerEvent) => set(e.target);
    const focus = (e: FocusEvent) => {
      // A focused control may disappear while its click changes the session or closes the drawer.
      if (e.target !== document.body && e.target !== document.documentElement) set(e.target);
    };
    document.addEventListener("pointerdown", pointer, true);
    document.addEventListener("focusin", focus, true);
    return () => {
      document.removeEventListener("pointerdown", pointer, true);
      document.removeEventListener("focusin", focus, true);
      delete pane.dataset.keyboardActive;
    };
  }, [embedded]);

  const saveSide = (next: { w: number; hidden: boolean }) => {
    setSide(next);
    try {
      localStorage.setItem(SIDE, JSON.stringify(next));
    } catch {
      // Kept for this run only.
    }
  };
  const toggle = useCallback(() => {
    setPeek(false);
    if (narrow) setFloatOpen((o) => !o);
    else saveSide({ ...side, hidden: !side.hidden });
  }, [narrow, side]);

  const go = (r: WsRoute) => {
    if (narrow) setFloatOpen(false);
    onRoute(r);
  };
  // A rename of the title belongs to the page it started on (also when ‹ › leave it).
  useEffect(() => setRenamingTitle(false), [route.host, route.session]);
  const openSession = (host: string, id: string) => {
    if (route.host === host && route.session === id) return;
    go({ host, session: id });
  };
  const newSession = (place?: Place) => go({ place: place ?? null });
  const toast = useCallback((x: Omit<Toast, "id">) => setToasts((l) => [...l.slice(-2), { ...x, id: Date.now() + Math.random() }]), []);

  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.isComposing || (embedded && !workspaceOwnsEvent(root.current, e))) return;
      if (e.key === "F2" && !e.metaKey && !e.ctrlKey && !e.altKey && current && !editable(e.target) && !(e.target as HTMLElement | null)?.closest?.(".ws-list")) {
        e.preventDefault();
        setRenamingTitle(true);
        return;
      }
      // SB-39: ⌘⌥P pin, ⌘⌥R rename, ⌘⌥U read / unread (Mark as done when it failed), ⌘⌥A archive — on the
      // row focused in the sidebar, else the open session. Alt changes `key` on a Mac, so `code` is read.
      if (modKey(e) && e.altKey && !e.shiftKey && SESSION_KEYS.has(e.code)) {
        const focused = (document.activeElement as HTMLElement | null)?.closest?.<HTMLElement>(".ws-list [data-row-key]")?.dataset.rowKey ?? null;
        const key = focused ?? current;
        const x = key ? workspace.items().find((i) => i.key === key) : null;
        if (!x) return;
        e.preventDefault();
        const ctx = { toast, onArchived: (k: string) => k === current && newSession(), onOpen: openSession };
        if (e.code === "KeyP") togglePin(x, ctx);
        else if (e.code === "KeyU") {
          if (workspace.dot(x) === "failed") markDone(x, ctx);
          else toggleRead(x);
        } else if (e.code === "KeyO") {
          // ⌘⌥O forks (SB-39); a session that cannot be forked now stays as it is.
          fork(x, ctx);
        } else if (e.code === "KeyA") {
          if (x.meta.archived) unarchive(x, ctx);
          else archive(x, ctx);
        } else if (focused) setRenamingRow(focused);
        else setRenamingTitle(true);
        return;
      }
      // ⌘⇧N: a new session where the open one runs (SB-07).
      if (modKey(e) && e.shiftKey && !e.altKey && e.code === "KeyN") {
        e.preventDefault();
        const x = current ? workspace.items().find((i) => i.key === current) : null;
        newSession(x ? { host: x.host, cwd: x.meta.cwd } : undefined);
        return;
      }
      if (!modKey(e) || e.altKey || e.shiftKey) return;
      const k = e.key.toLowerCase();
      if (k === "k") {
        e.preventDefault();
        setPalette((p) => !p);
      } else if (k === "b") {
        e.preventDefault();
        toggle();
      } else if (k === "n") {
        e.preventDefault();
        newSession();
      }
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  });

  // SB-03's edge peek (the simple path): with the sidebar folded in a narrow window, the pointer within
  // 48 px of the left edge shows a hint, within 16 px for 100 ms opens it over the page — not while a
  // button is held. It goes 180 ms after the pointer leaves it, later while one of its menus is open or
  // its rename field has the focus.
  useEffect(() => {
    if (!narrow || floatOpen) {
      setEdgeHint(false);
      return;
    }
    let timer: ReturnType<typeof setTimeout> | null = null;
    const stopTimer = () => {
      if (timer) clearTimeout(timer);
      timer = null;
    };
    const move = (e: PointerEvent) => {
      const bounds = root.current?.getBoundingClientRect();
      const x = e.clientX - (bounds?.left ?? 0);
      const inside = !!bounds && x >= 0 && x < bounds.width && e.clientY >= bounds.top && e.clientY < bounds.bottom;
      setEdgeHint(inside && !e.buttons && x <= 48);
      if (!inside || e.buttons || x > 16) return stopTimer();
      timer ??= setTimeout(() => {
        setPeek(true);
        setFloatOpen(true);
      }, 100);
    };
    const out = () => {
      stopTimer();
      setEdgeHint(false);
    };
    window.addEventListener("pointermove", move);
    const el = root.current;
    el?.addEventListener("pointerleave", out);
    return () => {
      stopTimer();
      window.removeEventListener("pointermove", move);
      el?.removeEventListener("pointerleave", out);
    };
  }, [narrow, floatOpen]);
  useEffect(() => {
    if (!peek || !floatOpen) return;
    const side = root.current?.querySelector<HTMLElement>(".ws-side");
    if (!side) return;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const busy = () => !!document.querySelector(".ws-menu") || (side.contains(document.activeElement) && document.activeElement instanceof HTMLInputElement);
    const later = () => {
      timer = setTimeout(() => {
        if (busy()) later();
        else {
          setFloatOpen(false);
          setPeek(false);
        }
      }, 180);
    };
    const enter = () => {
      if (timer) clearTimeout(timer);
      timer = null;
    };
    const leave = () => {
      enter();
      later();
    };
    side.addEventListener("pointerenter", enter);
    side.addEventListener("pointerleave", leave);
    return () => {
      enter();
      side.removeEventListener("pointerenter", enter);
      side.removeEventListener("pointerleave", leave);
    };
  }, [peek, floatOpen]);
  useEffect(() => {
    if (!narrow) setPeek(false);
  }, [narrow]);

  // The edge: drag to resize (3 px before it counts), below half the minimum it hides; a click hides
  // or shows; a double click goes back to 288 px (SB-02).
  const onGripDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const startX = e.clientX;
    const startW = width;
    let moved = false;
    let hide = false;
    let w = startW;
    const el = e.currentTarget;
    el.setPointerCapture(e.pointerId);
    const move = (m: PointerEvent) => {
      const dx = m.clientX - startX;
      if (!moved && Math.abs(dx) <= 3) return;
      if (!moved) setDragging(true);
      moved = true;
      hide = startW + dx < SIDEBAR.MIN / 2;
      w = clampSidebar(startW + dx, winW);
      setSide((s) => ({ ...s, w, hidden: hide }));
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
      setDragging(false);
      if (!moved) return;
      saveSide({ w: hide ? startW : w, hidden: hide });
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  };

  const corner = <Corner onToggle={toggle} back={back} forward={forward} embedded={embedded} />;
  const sidebar = (
    <Sidebar
      snap={snap}
      computers={computers}
      corner={corner}
      mode="agents"
      onMode={onMode}
      current={current}
      onOpen={openSession}
      onNew={newSession}
      onPalette={() => setPalette(true)}
      onPair={onPair}
      onSettings={onSettings}
      onArchived={(key) => key === current && newSession()}
      device={device}
      toast={toast}
      renaming={renamingRow}
      setRenaming={setRenamingRow}
    />
  );
  const mainCorner = docked ? null : corner;

  return (
    <div
      ref={root}
      className={cx("ws", dragging && "dragging")}
      data-embedded={embedded ? "true" : undefined}
      data-side={docked ? "docked" : shown ? "float" : "hidden"}
      style={{ ["--ws-side-w" as string]: `${width}px` }}
    >
      {shown && sidebar}
      {docked && (
        <div
          className="ws-grip"
          role="separator"
          aria-orientation="vertical"
          aria-label={t("ws_resize_sidebar")}
          aria-valuemin={SIDEBAR.MIN}
          aria-valuemax={SIDEBAR.MAX}
          aria-valuenow={width}
          tabIndex={0}
          title={t("ws_resize_sidebar")}
          onPointerDown={onGripDown}
          onDoubleClick={() => saveSide({ w: SIDEBAR.DEFAULT, hidden: false })}
          onKeyDown={(e) => {
            if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
              e.preventDefault();
              saveSide({ ...side, w: clampSidebar(width + (e.key === "ArrowRight" ? 16 : -16), winW) });
            } else if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              toggle();
            }
          }}
        />
      )}
      {narrow && floatOpen && !peek && <div className="ws-float-scrim" onPointerDown={() => setFloatOpen(false)} />}
      {edgeHint && <div className="ws-peek-hint" aria-hidden />}
      <main className="ws-main">
        {route.host && route.session ? (
          <SessionPage
            key={`${route.host}/${route.session}`}
            snap={snap}
            computers={computers}
            host={route.host}
            id={route.session}
            ticket={tickets.current.get(`${route.host}/${route.session}`) ?? (initialTarget?.host === route.host && initialTarget.session === route.session ? initialTarget.ticket : null) ?? null}
            corner={mainCorner}
            renaming={renamingTitle}
            setRenaming={setRenamingTitle}
            onWatch={onWatch}
            embedded={embedded}
            onLeave={() => newSession()}
            onArchived={(key) => key === current && newSession()}
            onNew={() => newSession()}
            onOpen={openSession}
            toast={toast}
          />
        ) : (
          <NewSession
            snap={snap}
            computers={computers}
            prefill={route.place ?? null}
            embedded={embedded}
            onPlaceChange={onPlaceChange}
            corner={mainCorner}
            onStarted={(host, session, ticket) => {
              tickets.current.set(`${host}/${session}`, ticket);
              go({ host, session });
            }}
            onOpen={(x: WsItem) => openSession(x.host, x.meta.id)}
            onPair={onPair}
            toast={toast}
          />
        )}
        <Toasts list={toasts} onDone={(id) => setToasts((l) => l.filter((x) => x.id !== id))} />
        <FamilyConfirm />
      </main>
      {palette && (
        <Palette
          snap={snap}
          onClose={() => setPalette(false)}
          onOpen={(x) => {
            setPalette(false);
            openSession(x.host, x.meta.id);
          }}
        />
      )}
    </div>
  );
}
