// While you drive, your mouse and keyboard on the picture go to the remote computer. With a host
// that has the live input channel, every move, press and wheel notch streams as it happens (and the
// clipboard text follows in both directions); with an older host, clicks, drags and keys become
// one `mouse` / `keyboard` tool call each, in order. Ctrl+Alt+H stays here: it hands back.

import { useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import { t } from "../lib/i18n";
import { createLiveInput, keyNameOf, type HostOs, type LiveInput, type MouseButton } from "../lib/live-input";
import { createManualInputDispatcher, type ManualInputDispatcher } from "../lib/manual-input";
import { detectOs } from "../lib/platform";

export interface ScreenRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** The host's key name for a browser key in the tool-call fallback, or null when it is only a modifier. */
function fallbackKeyName(e: React.KeyboardEvent, remote: HostOs): string | null {
  if (["Control", "Shift", "Alt", "Meta"].includes(e.key)) return null;
  const name = keyNameOf(e, "other", remote);
  if (!name) return null;
  return name.length === 1 ? name.toLowerCase() : name;
}

/** The `buttons` bit of each mouse button, and its host name. */
const BUTTON_BITS: Array<[number, MouseButton]> = [[1, "left"], [2, "right"], [4, "middle"]];
/** `PointerEvent.button` to its `buttons` bit. */
const bitOf = (button: number) => (button === 0 ? 1 : button === 2 ? 2 : button === 1 ? 4 : 0);
/** How often this computer's clipboard is read while you drive (and whenever the app gets focus). */
const CLIP_POLL_MS = 1000;
/** Largest clipboard text sent to the host, in UTF-8 bytes (the host's limit). */
const MAX_CLIP_BYTES = 1 << 20;
/** How often the stream says it is alive (the host lets go of held keys after 3 s of silence). */
const HEARTBEAT_MS = 1000;
/** Times a live stream that ended is reopened before falling back to one call per action. */
const RECONNECTS = 3;
const MODIFIER_KEYS = new Set(["ctrl", "shift", "alt", "meta"]);

export function RemoteInput({
  host,
  screen,
  mac,
  remotePointer,
  onHandBack,
  onError,
  onNotice,
  onRemoteFiles,
  onLocalFiles,
}: {
  host: string;
  screen: ScreenRect | null;
  /** The remote is a Mac (the command key is `cmd` there). */
  mac: boolean;
  /** The host draws its pointer on this side, so the local cursor is hidden over the picture. */
  remotePointer?: boolean;
  onHandBack: () => void;
  onError?: (message: string | null) => void;
  /** A passing note (input paused and resumed, a refused chord). */
  onNotice?: (message: string | null) => void;
  /** Files on the host's clipboard (their paths there); an empty list when it holds none. */
  onRemoteFiles?: (paths: string[]) => void;
  /** Files on this computer's clipboard. */
  onLocalFiles?: (paths: string[]) => void;
}) {
  const layer = useRef<HTMLDivElement>(null);
  const remote: HostOs = mac ? "macos" : "windows";
  const local: HostOs = detectOs() === "mac" ? "macos" : "windows";
  const callbacks = useRef({ onHandBack, onError, onNotice, onRemoteFiles, onLocalFiles });
  callbacks.current = { onHandBack, onError, onNotice, onRemoteFiles, onLocalFiles };

  // Live stream (the host's input channel), or null while the fallback is in use.
  const live = useRef<LiveInput | null>(null);
  const [isLive, setLive] = useState(false);
  // Keys held through the live stream, by the browser's `code`, with the name sent on press.
  const codes = useRef(new Map<string, string>());
  // Mouse buttons held through the live stream (`PointerEvent.buttons` bits).
  const buttonsDown = useRef(0);

  // Fallback: one tool call per action.
  const down = useRef<{ x: number; y: number; button: number } | null>(null);
  const text = useRef("");
  const flush = useRef<number | null>(null);
  const wheelAt = useRef(0);
  const dispatcher = useRef<ManualInputDispatcher | null>(null);

  const cancelPending = () => {
    if (flush.current !== null) clearTimeout(flush.current);
    flush.current = null;
    text.current = "";
    down.current = null;
    dispatcher.current?.cancel();
    if (live.current && (codes.current.size || buttonsDown.current)) live.current.releaseAll();
    codes.current.clear();
    buttonsDown.current = 0;
  };

  useEffect(() => {
    const input = createManualInputDispatcher(
      (tool, args) => api.callTool(host, tool, args, true),
      (failure) => {
        cancelPending();
        callbacks.current.onError?.(failure.reason === "overflow" ? t("manual_input_overflow") : t("manual_input_failed", { error: failure.message }));
      },
    );
    dispatcher.current = input;
    layer.current?.focus();

    let handle: { close(): void } | null = null;
    let stopped = false;
    let attempts = 0;
    let reconnect: number | null = null;
    let poll: number | null = null;
    let heartbeat: number | null = null;
    // The host takes this computer's clipboard text on this stream.
    let mayWriteClipboard = false;
    // The first read only records this computer's clipboard: only changes go to the host, so
    // taking over never replaces what the host's clipboard holds (the host does the same).
    let primed = false;
    let lastLocalClip: string | null = null;
    let lastHostClip: string | null = null;
    let lastLocalFiles = "";

    // This computer's clipboard goes to the host when it changes while the app is in front.
    const readClipboard = async () => {
      if (stopped || !document.hasFocus()) return;
      try {
        const [textNow, files] = await Promise.all([api.clipboardText(), api.clipboardFiles()]);
        if (stopped) return;
        const key = files.join("\n");
        if (key !== lastLocalFiles) {
          lastLocalFiles = key;
          callbacks.current.onLocalFiles?.(files);
        }
        if (!primed) {
          primed = true;
          lastLocalClip = textNow;
          return;
        }
        if (textNow === null || textNow === lastLocalClip || files.length > 0) return;
        lastLocalClip = textNow;
        const stream = live.current;
        if (!stream || !mayWriteClipboard || textNow === lastHostClip) return;
        if (new TextEncoder().encode(textNow).length > MAX_CLIP_BYTES) {
          callbacks.current.onNotice?.(t("clip_too_long"));
          return;
        }
        stream.clip(textNow);
      } catch {
        // A clipboard held by another app; next time.
      }
    };
    const onFocus = () => void readClipboard();

    const connect = () => {
      reconnect = null;
      // Lines from the host: clipboard changes, refusals, pauses; "" when the stream ends.
      const onLine = (line: string) => {
        if (stopped) return;
        if (line === "") {
          live.current = null;
          codes.current.clear();
          buttonsDown.current = 0;
          setLive(false);
          // Reopened a few times (a quick hand-back and take-over, a network blip); then one call
          // per action, which still works.
          if (attempts < RECONNECTS) reconnect = window.setTimeout(connect, 500 * ++attempts);
          else callbacks.current.onNotice?.(t("live_input_ended"));
          return;
        }
        let v: { clip?: string; clip_files?: string[]; err?: string; blocked?: string | null };
        try {
          v = JSON.parse(line);
        } catch {
          return;
        }
        if (typeof v.clip === "string") {
          lastHostClip = v.clip;
          lastLocalClip = v.clip;
          void api.setClipboardText(v.clip).catch(() => {});
        }
        if (Array.isArray(v.clip_files)) callbacks.current.onRemoteFiles?.(v.clip_files.filter((p) => typeof p === "string"));
        if (typeof v.err === "string") callbacks.current.onNotice?.(t("live_input_refused", { error: v.err }));
        if ("blocked" in v) callbacks.current.onNotice?.(v.blocked ? t("live_input_paused", { why: v.blocked }) : null);
      };
      api
        .openInput(host, { clipboard: true }, onLine)
        .then((h) => {
          if (stopped) {
            h.close();
            return;
          }
          handle = h;
          attempts = 0;
          mayWriteClipboard = h.clipboardWrite;
          live.current = createLiveInput((data) => void h.send(data), { pxPerNotch: local === "macos" ? 50 : 100 });
          setLive(true);
          callbacks.current.onError?.(null);
          heartbeat ??= window.setInterval(() => live.current?.heartbeat(), HEARTBEAT_MS);
          if (poll === null) {
            void readClipboard();
            poll = window.setInterval(() => void readClipboard(), CLIP_POLL_MS);
            if (typeof window.addEventListener === "function") window.addEventListener("focus", onFocus);
          }
        })
        .catch((e) => {
          // An older host, or input refused (policy, not driving): the tool calls below still work.
          if (!stopped) callbacks.current.onNotice?.(t("live_input_unavailable", { error: e instanceof Error ? e.message : String(e) }));
        });
    };
    connect();
    return () => {
      stopped = true;
      if (poll !== null) clearInterval(poll);
      if (heartbeat !== null) clearInterval(heartbeat);
      if (reconnect !== null) clearTimeout(reconnect);
      if (typeof window.removeEventListener === "function") window.removeEventListener("focus", onFocus);
      cancelPending();
      input.cancel();
      live.current?.close();
      live.current = null;
      handle?.close();
      if (dispatcher.current === input) dispatcher.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host]);

  // A stream restart or display geometry change invalidates gestures and unsent input.
  useEffect(() => {
    cancelPending();
    return cancelPending;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [screen?.x, screen?.y, screen?.width, screen?.height]);

  const call = (tool: string, args: Record<string, unknown>) => {
    if (dispatcher.current?.enqueue(tool, args)) callbacks.current.onError?.(null);
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

  /**
   * Live path: moves the pointer there, then presses and releases buttons until the host holds
   * exactly `buttons`. A second button pressed while one is held arrives as a pointermove, not a
   * pointerdown (Pointer Events), so every pointer event syncs the full set.
   */
  const syncPointer = (stream: LiveInput, e: React.PointerEvent, buttons: number) => {
    const p = toScreen(e.clientX, e.clientY);
    if (p) stream.move(p[0], p[1]);
    const before = buttonsDown.current;
    for (const [bit, name] of BUTTON_BITS) {
      if (buttons & bit && !(before & bit)) stream.button(name, true);
      if (!(buttons & bit) && before & bit) stream.button(name, false);
    }
    buttonsDown.current = buttons & 7;
  };

  const sendText = () => {
    if (text.current) call("keyboard", { text: text.current });
    text.current = "";
    flush.current = null;
  };

  const handBack = () => {
    cancelPending();
    callbacks.current.onHandBack();
  };

  return (
    <div
      ref={layer}
      tabIndex={0}
      aria-label="Remote screen"
      data-live={isLive ? "1" : undefined}
      onBlur={cancelPending}
      style={{ position: "absolute", inset: 0, zIndex: 5, outline: "none", cursor: isLive && remotePointer ? "none" : "default" }}
      onPointerDown={(e) => {
        layer.current?.focus();
        e.currentTarget.setPointerCapture(e.pointerId);
        const stream = live.current;
        if (stream) {
          syncPointer(stream, e, typeof e.buttons === "number" ? e.buttons : buttonsDown.current | bitOf(e.button));
          return;
        }
        down.current = { x: e.clientX, y: e.clientY, button: e.button };
      }}
      onPointerMove={(e) => {
        const stream = live.current;
        if (!stream) return;
        syncPointer(stream, e, typeof e.buttons === "number" ? e.buttons : buttonsDown.current);
      }}
      onPointerCancel={cancelPending}
      onPointerUp={(e) => {
        const stream = live.current;
        if (stream) {
          syncPointer(stream, e, typeof e.buttons === "number" ? e.buttons : buttonsDown.current & ~bitOf(e.button));
          return;
        }
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
        } else {
          // PointerEvent.detail is zero for real mouse input; each release is one click.
          call("mouse", { action: "click", x: b[0], y: b[1], space: "screen" });
        }
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        if (live.current) return;
        const p = toScreen(e.clientX, e.clientY);
        if (p) call("mouse", { action: "right_click", x: p[0], y: p[1], space: "screen" });
      }}
      onWheel={(e) => {
        const stream = live.current;
        if (stream) {
          const p = toScreen(e.clientX, e.clientY);
          if (p) stream.move(p[0], p[1]);
          stream.wheel(e.deltaX, e.deltaY, e.deltaMode);
          return;
        }
        const now = Date.now();
        if (now - wheelAt.current < 120) return;
        wheelAt.current = now;
        const p = toScreen(e.clientX, e.clientY);
        if (!p) return;
        const vertical = Math.abs(e.deltaY) >= Math.abs(e.deltaX);
        const direction = vertical ? (e.deltaY > 0 ? "down" : "up") : e.deltaX > 0 ? "right" : "left";
        call("mouse", { action: "scroll", x: p[0], y: p[1], space: "screen", direction, amount: 3 });
      }}
      onCompositionEnd={(e) => {
        // An input method's result (Chinese, Japanese, ...): typed as text on the host.
        const stream = live.current;
        if (stream && e.data) stream.text(e.data);
      }}
      onKeyDown={(e) => {
        if (e.ctrlKey && e.altKey && e.key.toLowerCase() === "h") {
          e.preventDefault();
          handBack();
          return;
        }
        // ⌃⌘F belongs to this app (full screen): the page handles it, the host never sees the F.
        if (e.ctrlKey && e.metaKey && e.key.toLowerCase() === "f") return;
        const stream = live.current;
        if (stream) {
          e.preventDefault();
          if (e.nativeEvent?.isComposing || e.key === "Process") return;
          // A Mac reports CapsLock as a keydown when it locks and a keyup when it unlocks: each is a tap.
          if (e.key === "CapsLock" && local === "macos") {
            stream.key("capslock", true);
            stream.key("capslock", false);
            return;
          }
          const code = e.code || e.key;
          // Auto-repeat presses the held key again on the host (Backspace, arrows held down).
          const held = e.repeat ? codes.current.get(code) : undefined;
          if (held) {
            stream.key(held, true);
            return;
          }
          const name = keyNameOf(e, local, remote);
          if (!name) return;
          codes.current.set(code, name);
          stream.key(name, true);
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
        const name = fallbackKeyName(e, remote);
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
      onKeyUp={(e) => {
        const stream = live.current;
        if (!stream) return;
        e.preventDefault();
        if (e.key === "CapsLock" && local === "macos") {
          stream.key("capslock", true);
          stream.key("capslock", false);
          return;
        }
        const code = e.code || e.key;
        const name = codes.current.get(code);
        codes.current.delete(code);
        if (name) stream.key(name, false);
        // Browsers deliver no key-up for keys released while the command key is held (macOS):
        // when it comes up, let go of the other keys too, but not of modifiers still held.
        if (e.key === "Meta") {
          for (const [heldCode, held] of codes.current) {
            if (MODIFIER_KEYS.has(held)) continue;
            stream.key(held, false);
            codes.current.delete(heldCode);
          }
        }
      }}
    />
  );
}
