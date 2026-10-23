// The window's keyboard shortcuts, as a pure table: a key event (or a mouse button) → an action.
// App.tsx listens on the window and acts; tools/desktop-nav.test.mjs runs this module in a vm, so it
// has only `import type`.
//
//   action                 macOS          Windows
//   back / forward         ⌘[ / ⌘]        Alt+← / Alt+→   (mouse buttons 3 / 4 on both)
//   settings               ⌘,             Ctrl+,
//   devices / approvals /  ⌘1 / ⌘2 / ⌘3   Ctrl+1 / 2 / 3
//   activity
//   pair a computer        ⌘N             Ctrl+N
//   search computers       ⌘F             Ctrl+F          (not inside the chat: it finds in the conversation)
//   sidebar                ⌘\ or ⌃⌘S      Ctrl+\
//   full screen            (⌃⌘F, Session) F11             (only on a session; forwarded to Session's ⌃⌘F)
//
// Nothing is taken while an input method is composing, while a dialog is open (data-modal), or from
// the remote picture (.nt-picture: while you drive, every key belongs to the remote computer).

import type { Os } from "./platform";

export type Action =
  | { kind: "back" }
  | { kind: "forward" }
  | { kind: "settings" }
  | { kind: "page"; page: "devices" | "approvals" | "activity" }
  | { kind: "pair" }
  | { kind: "search" }
  | { kind: "sidebar" }
  | { kind: "fullscreen" };

/** The parts of a KeyboardEvent the table reads. */
export interface KeyLike {
  key: string;
  code?: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  isComposing?: boolean;
  keyCode?: number;
  defaultPrevented?: boolean;
  repeat?: boolean;
}

/** Where the event happened. */
export interface KeyContext {
  /** The target is inside the remote picture (.nt-picture). */
  inPicture?: boolean;
  /** A dialog is open (<html data-modal>). */
  modal?: boolean;
  /** The target is inside the coding-agent chat (.nt-chat), which has its own ⌘F. */
  inChat?: boolean;
  /** The current route is a live session (F11 means something only there). */
  session?: boolean;
}

const PAGES = { "1": "devices", "2": "approvals", "3": "activity" } as const;

/** The key by its character; the physical key only when the layout types something else there
 *  (a Cyrillic or Greek layout), so a Dvorak "u" never counts as "f". */
const printable = (k: string) => /^[\x20-\x7e]$/.test(k);
const is = (e: KeyLike, key: string, code: string) => e.key.toLowerCase() === key || (!printable(e.key) && e.code === code);

/** The action a key press asks for, or null to leave it alone. */
export function shortcutOf(e: KeyLike, os: Os, ctx: KeyContext = {}): Action | null {
  if (e.defaultPrevented || e.isComposing || e.keyCode === 229) return null;
  if (ctx.inPicture || ctx.modal) return null;
  const digit = /^[123]$/.test(e.key) ? e.key : !printable(e.key) ? (/^Digit([123])$/.exec(e.code ?? "")?.[1] ?? null) : null;
  if (os === "mac") {
    if (e.metaKey && e.ctrlKey && !e.altKey && !e.shiftKey && is(e, "s", "KeyS")) return { kind: "sidebar" };
    if (!e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return null;
    if (is(e, "[", "BracketLeft")) return { kind: "back" };
    if (is(e, "]", "BracketRight")) return { kind: "forward" };
    if (is(e, ",", "Comma")) return { kind: "settings" };
    if (is(e, "\\", "Backslash")) return { kind: "sidebar" };
    if (is(e, "n", "KeyN")) return { kind: "pair" };
    if (is(e, "f", "KeyF")) return ctx.inChat ? null : { kind: "search" };
    if (digit) return { kind: "page", page: PAGES[digit as keyof typeof PAGES] };
    return null;
  }
  if (e.key === "F11" && !e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey) return ctx.session ? { kind: "fullscreen" } : null;
  if (e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey) {
    if (e.key === "ArrowLeft") return { kind: "back" };
    if (e.key === "ArrowRight") return { kind: "forward" };
    return null;
  }
  if (!e.ctrlKey || e.altKey || e.metaKey || e.shiftKey) return null;
  if (is(e, ",", "Comma")) return { kind: "settings" };
  if (is(e, "\\", "Backslash")) return { kind: "sidebar" };
  if (is(e, "n", "KeyN")) return { kind: "pair" };
  if (is(e, "f", "KeyF")) return ctx.inChat ? null : { kind: "search" };
  if (digit) return { kind: "page", page: PAGES[digit as keyof typeof PAGES] };
  return null;
}

/** The extra mouse buttons: 3 goes back, 4 forward (not over the remote picture). */
export function mouseActionOf(button: number, ctx: KeyContext = {}): Action | null {
  if (ctx.inPicture || ctx.modal) return null;
  if (button === 3) return { kind: "back" };
  if (button === 4) return { kind: "forward" };
  return null;
}

/** The hint shown next to a command (titles, tooltips). */
export function shortcutLabel(action: Action["kind"] | "page1" | "page2" | "page3", os: Os): string {
  const mac = os === "mac";
  const map: Record<string, string> = mac
    ? { back: "⌘[", forward: "⌘]", settings: "⌘,", pair: "⌘N", search: "⌘F", sidebar: "⌘\\", fullscreen: "⌃⌘F", page1: "⌘1", page2: "⌘2", page3: "⌘3" }
    : { back: "Alt+←", forward: "Alt+→", settings: "Ctrl+,", pair: "Ctrl+N", search: "Ctrl+F", sidebar: "Ctrl+\\", fullscreen: "F11", page1: "Ctrl+1", page2: "Ctrl+2", page3: "Ctrl+3" };
  return map[action] ?? "";
}
