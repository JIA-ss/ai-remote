// Your mouse and keyboard as a live stream of events to the host (its `input/1` channel), while you
// drive. Events are JSON lines batched every few milliseconds into one write; runs of pointer moves
// collapse to the last one, so a slow link never banks movement; the wheel's pixel deltas turn
// into notches. Pure (no DOM), so it is unit-tested in tools/live-input.test.mjs.

export type HostOs = "windows" | "macos" | "other";
export type MouseButton = "left" | "right" | "middle";

export interface LiveInput {
  move(x: number, y: number): void;
  button(button: MouseButton, down: boolean): void;
  /** A browser wheel event's deltas and deltaMode (0 pixels, 1 lines, 2 pages). */
  wheel(deltaX: number, deltaY: number, deltaMode: number): void;
  /** `name` is the host's key name (see `keyNameOf`). */
  key(name: string, down: boolean): void;
  /** Text an input method composed. */
  text(text: string): void;
  /** Replaces the host's clipboard text. */
  clip(text: string): void;
  /** Says the controller is still there (the host lets go of held keys after 3 s of silence). */
  heartbeat(): void;
  /** The host lets go of everything this stream holds. */
  releaseAll(): void;
  flush(): void;
  /** Releases everything and sends nothing more. */
  close(): void;
  /** Host key names currently held through this stream. */
  readonly held: ReadonlySet<string>;
}

/** Wheel pixels per notch by default: Chromium (WebView2) reports 100 per notch of a mouse wheel;
 * WebKit reports less, so the Mac uses a smaller step (see `pxPerNotch`). Trackpads send many
 * small deltas either way. */
const PX_PER_NOTCH = 100;
const LINES_PER_NOTCH = 3;
const NOTCHES_PER_PAGE = 3;
/** Most notches one line carries on an axis (the host refuses more). */
const MAX_NOTCHES = 50;

export interface LiveInputOptions {
  /** How long lines wait for company before they are sent (default 4 ms). */
  flushMs?: number;
  /** Wheel pixels that make one notch (default 100). */
  pxPerNotch?: number;
  schedule?: (fn: () => void, ms: number) => unknown;
  cancel?: (handle: unknown) => void;
}

export function createLiveInput(send: (data: string) => void, options: LiveInputOptions = {}): LiveInput {
  const flushMs = options.flushMs ?? 4;
  const pxPerNotch = options.pxPerNotch ?? PX_PER_NOTCH;
  const schedule = options.schedule ?? ((fn, ms) => setTimeout(fn, ms));
  const cancel = options.cancel ?? ((h) => clearTimeout(h as number));
  const lines: string[] = [];
  let lastIsMove = false;
  let timer: unknown = null;
  let closed = false;
  let wheelX = 0, wheelY = 0;
  const held = new Set<string>();

  const flush = () => {
    if (timer !== null) cancel(timer);
    timer = null;
    if (lines.length === 0) return;
    send(lines.join("\n") + "\n");
    lines.length = 0;
    lastIsMove = false;
  };
  const queue = (line: string, isMove = false) => {
    if (closed) return;
    if (isMove && lastIsMove) lines[lines.length - 1] = line;
    else lines.push(line);
    lastIsMove = isMove;
    if (timer === null) timer = schedule(flush, flushMs);
  };

  return {
    held,
    move(x, y) {
      queue(JSON.stringify({ m: [Math.round(x), Math.round(y)] }), true);
    },
    button(button, down) {
      queue(JSON.stringify({ b: button, down }));
    },
    wheel(deltaX, deltaY, deltaMode) {
      if (closed) return;
      const scale = deltaMode === 1 ? 1 / LINES_PER_NOTCH : deltaMode === 2 ? NOTCHES_PER_PAGE : 1 / pxPerNotch;
      wheelX += deltaX * scale;
      wheelY += deltaY * scale;
      // Whole notches go in order with the other events; the rest waits for more movement.
      const nx = Math.trunc(wheelX), ny = Math.trunc(wheelY);
      if (nx === 0 && ny === 0) return;
      wheelX -= nx;
      wheelY -= ny;
      queue(JSON.stringify({ w: [clamp(nx), clamp(ny)] }));
    },
    key(name, down) {
      if (down) held.add(name);
      else held.delete(name);
      queue(JSON.stringify({ k: name, down }));
    },
    text(text) {
      if (text) queue(JSON.stringify({ t: text }));
    },
    clip(text) {
      queue(JSON.stringify({ clip: text }));
    },
    heartbeat() {
      queue(JSON.stringify({ hb: 1 }));
    },
    releaseAll() {
      held.clear();
      queue(JSON.stringify({ release: true }));
    },
    flush,
    close() {
      if (closed) return;
      if (held.size) this.releaseAll();
      flush();
      closed = true;
    },
  };
}

function clamp(n: number): number {
  return Math.max(-MAX_NOTCHES, Math.min(MAX_NOTCHES, n));
}

/** Browser `KeyboardEvent.key` values that are not a single character, by the host's key names. */
const KEY_NAMES: Record<string, string> = {
  Enter: "enter",
  Backspace: "backspace",
  Tab: "tab",
  Escape: "escape",
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
  CapsLock: "capslock",
  PrintScreen: "printscreen",
  ScrollLock: "scrolllock",
  NumLock: "numlock",
  Pause: "pause",
  ContextMenu: "menu",
  Control: "ctrl",
  Shift: "shift",
  Alt: "alt",
  Meta: "meta",
  AudioVolumeUp: "volumeup",
  AudioVolumeDown: "volumedown",
  AudioVolumeMute: "volumemute",
  MediaPlayPause: "playpause",
  MediaTrackNext: "medianext",
  MediaTrackPrevious: "mediaprev",
};

/**
 * The host's name for a browser key, or null for keys with no remote meaning (dead keys, AltGr
 * itself, an input method composing). `local` and `remote` map the command key between a Mac and
 * a PC: Cmd on a Mac driving a Windows host acts as Ctrl (Cmd+C copies there too), Ctrl on a PC
 * driving a Mac acts as Cmd. With Ctrl, Alt or Cmd held a letter or digit is named by its
 * position (`code`), not the character the local layout makes of it: Ctrl+C on a Russian layout,
 * or Option+A on a Mac (which would type å), still mean that key on the host.
 */
export function keyNameOf(
  e: { key: string; code?: string; ctrlKey?: boolean; altKey?: boolean; metaKey?: boolean },
  local: HostOs,
  remote: HostOs,
): string | null {
  if (e.ctrlKey || e.altKey || e.metaKey) {
    const letter = /^Key([A-Z])$/.exec(e.code ?? "");
    if (letter) return letter[1].toLowerCase();
    const digit = /^Digit([0-9])$/.exec(e.code ?? "");
    if (digit) return digit[1];
  }
  const name = KEY_NAMES[e.key];
  if (name === "meta") return local === "macos" && remote === "windows" ? "ctrl" : "meta";
  if (name === "ctrl") return local === "windows" && remote === "macos" ? "meta" : "ctrl";
  if (name) return name;
  if (/^F\d{1,2}$/.test(e.key)) return e.key.toLowerCase();
  if (e.key.length === 1) return e.key;
  return null;
}
