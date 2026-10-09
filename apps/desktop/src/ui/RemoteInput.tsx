// While you drive, your mouse and keyboard on the picture go to the remote computer: clicks, drags
// and the wheel become the host's mouse tool in screen coordinates, keys its keyboard tool. Each call
// is marked as yours, so it runs while the agent waits. Ctrl+Alt+H stays here: it hands back.

import { useEffect, useRef } from "react";
import { api } from "../lib/api";

export interface ScreenRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

const KEY_NAMES: Record<string, string> = {
  Enter: "enter",
  Backspace: "backspace",
  Tab: "tab",
  Escape: "esc",
  Delete: "delete",
  Home: "home",
  End: "end",
  PageUp: "pageup",
  PageDown: "pagedown",
  ArrowUp: "up",
  ArrowDown: "down",
  ArrowLeft: "left",
  ArrowRight: "right",
  " ": "space",
  Insert: "insert",
};

/** The host's key name for a browser key, or null when it is only a modifier. */
function keyName(e: KeyboardEvent | React.KeyboardEvent): string | null {
  if (["Control", "Shift", "Alt", "Meta"].includes(e.key)) return null;
  if (KEY_NAMES[e.key]) return KEY_NAMES[e.key];
  if (/^F\d{1,2}$/.test(e.key)) return e.key.toLowerCase();
  if (e.key.length === 1) return e.key.toLowerCase();
  return null;
}

export function RemoteInput({
  host,
  screen,
  mac,
  onHandBack,
}: {
  host: string;
  screen: ScreenRect | null;
  /** The remote is a Mac (the command key is `cmd` there). */
  mac: boolean;
  onHandBack: () => void;
}) {
  const layer = useRef<HTMLDivElement>(null);
  const down = useRef<{ x: number; y: number; button: number } | null>(null);
  const text = useRef("");
  const flush = useRef<number | null>(null);
  const wheelAt = useRef(0);

  // Keyboard focus moves to the picture when you take over.
  useEffect(() => {
    layer.current?.focus();
  }, []);

  const call = (tool: string, args: Record<string, unknown>) => {
    void api.callTool(host, tool, args, true).catch(() => {});
  };

  /** A point of the layer in screen coordinates. */
  const toScreen = (clientX: number, clientY: number): [number, number] | null => {
    const el = layer.current;
    if (!el || !screen) return null;
    const r = el.getBoundingClientRect();
    const fx = Math.min(Math.max((clientX - r.left) / r.width, 0), 1);
    const fy = Math.min(Math.max((clientY - r.top) / r.height, 0), 1);
    return [Math.round(screen.x + fx * (screen.width - 1)), Math.round(screen.y + fy * (screen.height - 1))];
  };

  const sendText = () => {
    if (text.current) call("keyboard", { text: text.current });
    text.current = "";
    flush.current = null;
  };

  return (
    <div
      ref={layer}
      tabIndex={0}
      aria-label="Remote screen"
      style={{ position: "absolute", inset: 0, zIndex: 5, outline: "none", cursor: "default" }}
      onPointerDown={(e) => {
        layer.current?.focus();
        down.current = { x: e.clientX, y: e.clientY, button: e.button };
      }}
      onPointerUp={(e) => {
        const start = down.current;
        down.current = null;
        if (!start || start.button === 2) return;
        const a = toScreen(start.x, start.y);
        const b = toScreen(e.clientX, e.clientY);
        if (!a || !b) return;
        const moved = Math.hypot(e.clientX - start.x, e.clientY - start.y) > 4;
        if (moved && start.button === 0) {
          call("mouse", { action: "drag", x: a[0], y: a[1], to_x: b[0], to_y: b[1], space: "screen" });
        } else if (start.button === 1) {
          call("mouse", { action: "middle_click", x: b[0], y: b[1], space: "screen" });
        } else if (e.detail === 2) {
          call("mouse", { action: "double_click", x: b[0], y: b[1], space: "screen" });
        } else if (e.detail === 1) {
          call("mouse", { action: "click", x: b[0], y: b[1], space: "screen" });
        }
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        const p = toScreen(e.clientX, e.clientY);
        if (p) call("mouse", { action: "right_click", x: p[0], y: p[1], space: "screen" });
      }}
      onWheel={(e) => {
        const now = Date.now();
        if (now - wheelAt.current < 120) return;
        wheelAt.current = now;
        const p = toScreen(e.clientX, e.clientY);
        if (!p) return;
        const vertical = Math.abs(e.deltaY) >= Math.abs(e.deltaX);
        const direction = vertical ? (e.deltaY > 0 ? "down" : "up") : e.deltaX > 0 ? "right" : "left";
        call("mouse", { action: "scroll", x: p[0], y: p[1], space: "screen", direction, amount: 3 });
      }}
      onKeyDown={(e) => {
        if (e.ctrlKey && e.altKey && e.key.toLowerCase() === "h") {
          e.preventDefault();
          onHandBack();
          return;
        }
        const plain = e.key.length === 1 && !e.ctrlKey && !e.altKey && !e.metaKey;
        e.preventDefault();
        if (plain) {
          // Characters are batched into text, which types in any layout or language.
          text.current += e.key;
          if (flush.current) clearTimeout(flush.current);
          flush.current = window.setTimeout(sendText, 60);
          return;
        }
        const name = keyName(e);
        if (!name) return;
        if (flush.current) {
          clearTimeout(flush.current);
          sendText();
        }
        const mods = [e.ctrlKey && "ctrl", e.altKey && "alt", e.shiftKey && name.length > 1 && "shift", e.metaKey && (mac ? "cmd" : "win")].filter(
          Boolean,
        );
        call("keyboard", { keys: [...mods, name].join("+") });
      }}
    />
  );
}
