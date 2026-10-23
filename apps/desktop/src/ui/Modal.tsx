// Window sheets live beside #root: page clipping, opacity and transforms cannot become their
// backdrop root. Only the top sheet handles keys; the app behind it is inert until the last closes.
import { useLayoutEffect, useRef, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";
import { cx } from "./util";

const sheets: HTMLElement[] = [];
let restore: (() => void) | undefined;

function syncSheets() {
  sheets.forEach((sheet, i) => { sheet.inert = i !== sheets.length - 1; });
  if (sheets.length) document.documentElement.dataset.modal = String(sheets.length);
}

export function Modal({
  label, children, onClose, dismissible = true, role = "dialog", className, size, initialFocus,
}: {
  label: string;
  children: ReactNode;
  onClose: () => void;
  dismissible?: boolean;
  role?: "dialog" | "alertdialog";
  className?: string;
  size?: "wide";
  initialFocus?: RefObject<HTMLElement | null>;
}) {
  const layer = useRef<HTMLDivElement>(null);
  const dialog = useRef<HTMLDivElement>(null);
  const latest = useRef({ onClose, dismissible, initialFocus });
  latest.current = { onClose, dismissible, initialFocus };

  useLayoutEffect(() => {
    const node = layer.current!;
    const sheet = dialog.current!;
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    if (!sheets.length) {
      const root = document.getElementById("root");
      const inert = root?.inert ?? false;
      const modal = document.documentElement.getAttribute("data-modal");
      const overflow = document.body.style.overflow;
      if (root) root.inert = true;
      document.body.style.overflow = "hidden";
      restore = () => {
        if (root) root.inert = inert;
        if (modal === null) document.documentElement.removeAttribute("data-modal");
        else document.documentElement.setAttribute("data-modal", modal);
        document.body.style.overflow = overflow;
      };
    }
    sheets.push(node);
    syncSheets();
    const top = () => sheets.at(-1) === node;
    const focusable = () => [...sheet.querySelectorAll<HTMLElement>(
      'button, input, select, textarea, a[href], [tabindex]',
    )].filter((el) => el.tabIndex >= 0 && !el.matches(":disabled, [inert], [inert] *") && el.getClientRects().length);
    const focus = () => (latest.current.initialFocus?.current ?? focusable()[0] ?? sheet).focus({ preventScroll: true });
    focus();
    const key = (e: KeyboardEvent) => {
      if (!top() || e.isComposing || e.keyCode === 229) return;
      if (e.key === "Escape") {
        // Even a sheet waiting for the system's install prompt owns Esc; never deny a request behind it.
        e.preventDefault();
        e.stopImmediatePropagation();
        if (!e.repeat && latest.current.dismissible) latest.current.onClose();
      } else if (e.key === "Tab") {
        const items = focusable();
        const at = items.indexOf(document.activeElement as HTMLElement);
        if (!items.length || at < 0 || (!e.shiftKey && at === items.length - 1) || (e.shiftKey && at === 0)) {
          e.preventDefault();
          (e.shiftKey ? items.at(-1) ?? sheet : items[0] ?? sheet).focus();
        }
      }
    };
    const keepFocus = (e: FocusEvent) => {
      if (top() && !sheet.contains(e.target as Node)) focus();
    };
    // Disabling or replacing the focused control can silently send focus to body (no focusin).
    // Keep a running install, and the pairing receipt that replaces its buttons, in the sheet.
    const checkFocus = () => {
      const active = document.activeElement;
      if (top() && (!sheet.contains(active) || active?.matches(":disabled, [hidden], [hidden] *"))) {
        sheet.focus({ preventScroll: true });
      }
    };
    const lostFocus = () => queueMicrotask(checkFocus);
    const observer = new MutationObserver(checkFocus);
    observer.observe(sheet, { subtree: true, childList: true, attributes: true, attributeFilter: ["disabled", "hidden"] });
    window.addEventListener("keydown", key, true);
    document.addEventListener("focusin", keepFocus);
    sheet.addEventListener("focusout", lostFocus);
    return () => {
      const wasTop = top();
      observer.disconnect();
      sheet.removeEventListener("focusout", lostFocus);
      window.removeEventListener("keydown", key, true);
      document.removeEventListener("focusin", keepFocus);
      sheets.splice(sheets.indexOf(node), 1);
      syncSheets();
      if (!sheets.length) { restore?.(); restore = undefined; }
      if (wasTop && previous?.isConnected) previous.focus({ preventScroll: true });
    };
  }, []);

  return createPortal(
    <div className="gl-modal-root" ref={layer}>
      <div className="smoke gl-scrim" aria-hidden="true" onClick={() => {
        if (sheets.at(-1) === layer.current && latest.current.dismissible) latest.current.onClose();
      }} />
      <div ref={dialog} className={cx("cdlg gl-sheet gl-glass thick", size === "wide" && "gl-sheet-wide", className)} role={role}
        aria-modal="true" aria-label={label} tabIndex={-1}>
        {children}
      </div>
    </div>,
    document.body,
  );
}
