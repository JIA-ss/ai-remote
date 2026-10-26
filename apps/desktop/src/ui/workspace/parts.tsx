// Small pieces the workspace's sidebar, pages and panels share: the window corner (traffic-light room,
// sidebar toggle, back / forward), the status dot, a computer's mark, the popup menu and toasts.

import { ArrowLeft, ArrowRight, Check, Laptop, LaptopMinimal, Monitor, PanelLeft } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import { t, type Key } from "../../lib/i18n";
import { detectOs } from "../../lib/platform";
import type { Dot } from "../../lib/workspace";
import { cx } from "../util";

export const isMac = detectOs() === "mac";
/** The shortcut modifier: ⌘ on macOS, Ctrl elsewhere. */
export const modKey = (e: { metaKey: boolean; ctrlKey: boolean }) => (isMac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey);
export const modLabel = isMac ? "⌘" : "Ctrl+";

/** The page's width and height in CSS pixels (the layout viewport, without scrollbars). */
export const viewWidth = () => document.documentElement.clientWidth || window.innerWidth;
export const viewHeight = () => document.documentElement.clientHeight || window.innerHeight;

/** Whether an embedded workspace owns a key. Removing a focused row or drawer sends focus back to
 *  body; keep the last pane's shortcuts working until a real pointer / focus action selects another
 *  pane. A concrete target outside the workspace always wins over that remembered ownership. */
export function workspaceOwnsEvent(pane: HTMLElement | null, e: { target: EventTarget | null }): boolean {
  if (!pane || pane.closest("[inert]")) return false;
  if (e.target instanceof Node && pane.contains(e.target)) return true;
  if (e.target !== document.body && e.target !== document.documentElement) return false;
  const focused = document.activeElement;
  if (focused && focused !== document.body && focused !== document.documentElement && !pane.contains(focused)) return false;
  return pane.dataset.keyboardActive === "true";
}

/** The window's top-left corner: room for the traffic lights (macOS), sidebar toggle, back, forward (SB-01). */
export function Corner({ onToggle, back, forward, embedded = false }: { onToggle: () => void; back: (() => void) | null; forward: (() => void) | null; embedded?: boolean }) {
  return (
    <div className="ws-corner">
      {isMac && !embedded && <span className="ws-lights" data-tauri-drag-region />}
      <button type="button" className="ws-ib" onClick={onToggle} aria-label={t("ws_toggle_sidebar")} title={`${t("ws_toggle_sidebar")} (${modLabel}B)`}>
        <PanelLeft size={16} strokeWidth={1.5} />
      </button>
      <button type="button" className="ws-ib" onClick={() => back?.()} disabled={!back} aria-label={t("ws_back")} title={t("ws_back")}>
        <ArrowLeft size={16} strokeWidth={1.5} />
      </button>
      <button type="button" className="ws-ib" onClick={() => forward?.()} disabled={!forward} aria-label={t("ws_forward")} title={t("ws_forward")}>
        <ArrowRight size={16} strokeWidth={1.5} />
      </button>
    </div>
  );
}

const DOT_LABEL: Record<Dot, Key> = { awaiting: "ws_dot_awaiting", failed: "ws_dot_failed", unread: "ws_dot_unread", running: "ws_dot_running" };
/** VS-12: every running dot blinks in step with the wall clock, also one that appears later. */
const BLINK_MS = 1200;

/** A session's status dot (6 px): amber needs you, red failed, blue unread, gray blinking running. */
export function StatusDot({ kind, className }: { kind: Dot; className?: string }) {
  return (
    <i
      className={cx("ws-dot", className)}
      data-kind={kind}
      role="img"
      aria-label={t(DOT_LABEL[kind])}
      title={t(DOT_LABEL[kind])}
      onAnimationStart={(e) => {
        for (const a of e.currentTarget.getAnimations()) a.startTime = -(performance.timeOrigin % BLINK_MS);
      }}
    />
  );
}

/** A computer's mark: a laptop for a Mac, a monitor otherwise, slashed while it is offline (SB-47). */
export function ComputerMark({ os, online, size = 14 }: { os: "windows" | "mac" | null; online: boolean | null; size?: number }) {
  if (online === false) return <LaptopMinimal size={size} strokeWidth={1.5} className="ws-off" />;
  const I = os === "mac" ? Laptop : Monitor;
  return <I size={size} strokeWidth={1.5} />;
}

/** Sets `data-overflowing` (and the full text as a tooltip) while the element's text is cut (VS-29). */
export function useOverflow<T extends HTMLElement>(text: string): RefObject<T | null> {
  const ref = useRef<T | null>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const check = () => {
      const over = el.scrollWidth > el.clientWidth + 1;
      if (over) {
        el.dataset.overflowing = "";
        el.title = text;
      } else {
        delete el.dataset.overflowing;
        el.removeAttribute("title");
      }
    };
    check();
    const ro = new ResizeObserver(check);
    ro.observe(el);
    return () => ro.disconnect();
  }, [text]);
  return ref;
}

export interface MenuItem {
  key: string;
  label: string;
  /** A letter that picks the item while the menu is open (SB-35). */
  letter?: string;
  danger?: boolean;
  disabled?: boolean;
  /** Why it is disabled, or a note under the label. */
  note?: string;
  /** Keep the menu open after it (a confirm step). */
  keep?: boolean;
  /** A line under the label (what a choice does), and a check for the current choice. */
  detail?: string;
  checked?: boolean;
  icon?: ReactNode;
  /** Takes the focus when the menu opens (instead of the checked item). */
  autofocus?: boolean;
  run: () => void;
}

/** A menu's line: an item, a separator, or a small heading. */
export type MenuLine = MenuItem | "sep" | { heading: string };

/** Where a menu opens: at a point (a right click), or beside an element, below it unless there is no room. */
export type MenuAt = { x: number; y: number } | { anchor: DOMRect; side?: "below" | "above" };

function place(at: MenuAt, w: number, h: number): { x: number; y: number } {
  const fit = (x: number, y: number) => ({ x: Math.max(8, Math.min(x, viewWidth() - w - 8)), y: Math.max(8, Math.min(y, viewHeight() - h - 8)) });
  if ("x" in at) return fit(at.x, at.y);
  const a = at.anchor;
  const below = a.bottom + 4;
  const above = a.top - h - 4;
  const roomBelow = below + h <= viewHeight() - 8;
  const roomAbove = above >= 8;
  const y = at.side === "above" ? (roomAbove || !roomBelow ? above : below) : roomBelow || !roomAbove ? below : above;
  return fit(a.left, y);
}

/**
 * A popup menu at a point (a right click) or beside an element: closes on Esc, a click elsewhere,
 * scrolling or leaving the window; ↑ / ↓ / Enter and the items' letters choose.
 */
export function PopMenu({ at, items, onClose, label, className, footer }: { at: MenuAt; items: MenuLine[]; onClose: () => void; label: string; className?: string; footer?: ReactNode }) {
  const box = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null);
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos(place(at, r.width, r.height));
  }, [at]);
  useLayoutEffect(() => {
    const el = box.current;
    // The measuring commit is hidden, so focus only after positioning makes the menu visible.
    // Item/confirmation updates keep the current focus instead of moving it back to the first button.
    if (!pos || !el) return;
    (el.querySelector<HTMLElement>("[data-autofocus]") ?? el.querySelector<HTMLButtonElement>("button[aria-checked=true]:not(:disabled)") ?? el.querySelector<HTMLButtonElement>("button:not(:disabled)"))?.focus();
  }, [pos]);
  useEffect(() => {
    const down = (e: Event) => {
      if (box.current && !box.current.contains(e.target as Node)) onClose();
    };
    const scroll = (e: Event) => {
      if (box.current && !box.current.contains(e.target as Node)) onClose();
    };
    document.addEventListener("pointerdown", down, true);
    document.addEventListener("scroll", scroll, true);
    window.addEventListener("blur", onClose);
    return () => {
      document.removeEventListener("pointerdown", down, true);
      document.removeEventListener("scroll", scroll, true);
      window.removeEventListener("blur", onClose);
    };
  }, [onClose]);
  const pick = (it: MenuItem) => {
    if (it.disabled) return;
    it.run();
    if (!it.keep) onClose();
  };
  const isItem = (x: MenuLine): x is MenuItem => x !== "sep" && !("heading" in x);
  return (
    <div
      ref={box}
      className={cx("ws-menu", className)}
      role="menu"
      aria-label={label}
      style={pos ? { left: pos.x, top: pos.y } : { left: 0, top: 0, visibility: "hidden" }}
      onKeyDown={(e) => {
        // A footer's field (the model menu's "another model") is a stop of ↑ / ↓ too.
        const buttons = [...(box.current?.querySelectorAll<HTMLElement>("button:not(:disabled), .ws-menu-field input") ?? [])];
        const i = buttons.indexOf(document.activeElement as HTMLElement);
        const typing = (e.target as HTMLElement | null)?.tagName === "INPUT";
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          onClose();
        } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
          e.preventDefault();
          const n = buttons.length;
          if (n) buttons[(i + (e.key === "ArrowDown" ? 1 : n - 1) + n) % n].focus();
        } else if (!typing && !e.metaKey && !e.ctrlKey && !e.altKey && e.key.length === 1) {
          const it = items.find((x): x is MenuItem => isItem(x) && x.letter === e.key.toLowerCase());
          if (it) {
            e.preventDefault();
            pick(it);
          }
        }
      }}
    >
      {items.map((it, i) =>
        it === "sep" ? (
          <div key={`sep${i}`} className="ws-menu-sep" role="separator" />
        ) : "heading" in it ? (
          <div key={`h${i}`} className="ws-menu-h">
            {it.heading}
          </div>
        ) : (
          <button
            key={it.key}
            type="button"
            role={it.checked === undefined ? "menuitem" : "menuitemradio"}
            aria-checked={it.checked}
            className={cx("ws-menu-item", it.danger && "danger", it.detail && "detailed")}
            disabled={it.disabled}
            data-autofocus={it.autofocus || undefined}
            title={it.disabled ? it.note : undefined}
            onClick={() => pick(it)}
          >
            {it.icon && <span className="ws-menu-icon">{it.icon}</span>}
            <span className="ws-menu-text">
              <span className="ws-menu-label">{it.label}</span>
              {(it.detail || (it.disabled && it.note)) && <span className="ws-menu-detail">{it.disabled && it.note ? it.note : it.detail}</span>}
            </span>
            {it.letter && <span className="ws-menu-key">{it.letter.toUpperCase()}</span>}
            {it.checked && <Check className="ws-menu-check" size={14} strokeWidth={1.75} />}
          </button>
        ),
      )}
      {footer}
    </div>
  );
}

export interface Toast {
  id: number;
  text: string;
  action?: { label: string; run: () => void };
  tone?: "error";
}

/** Toasts at the bottom of the workspace; each leaves after a while (6 s, 8 s for errors). */
export function Toasts({ list, onDone }: { list: Toast[]; onDone: (id: number) => void }) {
  return (
    <div className="ws-toasts" role="status" aria-live="polite">
      {list.map((x) => (
        <ToastRow key={x.id} toast={x} onDone={() => onDone(x.id)} />
      ))}
    </div>
  );
}

function ToastRow({ toast, onDone }: { toast: Toast; onDone: () => void }) {
  useEffect(() => {
    const id = setTimeout(onDone, toast.tone === "error" ? 8000 : 6000);
    return () => clearTimeout(id);
  }, []);
  return (
    <div className={cx("ws-toast", toast.tone)}>
      <span className="ws-toast-text">{toast.text}</span>
      {toast.action && (
        <button
          type="button"
          className="ws-toast-act"
          onClick={() => {
            toast.action?.run();
            onDone();
          }}
        >
          {toast.action.label}
        </button>
      )}
    </div>
  );
}

/** Shows `children` only after `ms` (SP-39: a load that is quick never flashes a placeholder). */
export function Later({ ms = 500, children }: { ms?: number; children: ReactNode }) {
  const [shown, setShown] = useState(false);
  useEffect(() => {
    const id = setTimeout(() => setShown(true), ms);
    return () => clearTimeout(id);
  }, [ms]);
  return shown ? <>{children}</> : null;
}
