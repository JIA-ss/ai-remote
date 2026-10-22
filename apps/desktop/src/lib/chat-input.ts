// The chat composer's input helpers: the `/` command menu and `@` file menu (what the caret is in,
// what matches, what choosing puts in the text), image attachments, finding text in the
// conversation and quoting a selection. Pure, so they are testable without a page
// (tools/chat-input.test.mjs).
//
// The behaviour follows Orca's native chat composer (stablyai/orca, MIT License, Copyright (c) 2026
// Lovecast Inc.): native-chat-composer-state's autocomplete (slash at the start of the message,
// `@` after a space), NativeChatAutocompleteMenus, NativeChatFindBar and NativeChatSelectionQuote.

/** A command or skill the agent offers (see the host's chat/event.rs `SlashCommand`). */
export interface SlashCommand {
  name: string;
  description?: string;
  kind: "command" | "skill" | string;
  /** What choosing it puts in the message (`/name`, or `$name` for a Codex skill). */
  insert: string;
  hint?: string;
}

/** The menu the caret is in: `/` at the start of the message, or `@` at the start of a word. */
export interface Autocomplete {
  mode: "slash" | "mention";
  /** What was typed after the trigger. */
  query: string;
  /** Where the trigger is, and the end of the word the caret is in. */
  start: number;
  end: number;
}

export function autocompleteAt(text: string, caret: number): Autocomplete | null {
  // The word around the caret: back to a space, forward to one.
  let start = caret;
  while (start > 0 && !/\s/.test(text[start - 1])) start--;
  let end = caret;
  while (end < text.length && !/\s/.test(text[end])) end++;
  const word = text.slice(start, caret);
  if (word.startsWith("/") && text.slice(0, start).trim() === "" && !word.slice(1).includes("/")) {
    return { mode: "slash", query: word.slice(1), start, end };
  }
  if (word.startsWith("@")) return { mode: "mention", query: word.slice(1), start, end };
  return null;
}

/** Commands matching `query`: names that start with it first, then names and descriptions that contain it. */
export function matchCommands(commands: SlashCommand[], query: string, limit = 50): SlashCommand[] {
  const q = query.toLowerCase();
  if (!q) return commands.slice(0, limit);
  const rank = (c: SlashCommand) => {
    const n = c.name.toLowerCase();
    if (n.startsWith(q)) return 0;
    if (n.includes(q)) return 1;
    if ((c.description ?? "").toLowerCase().includes(q)) return 2;
    return 3;
  };
  return commands
    .map((c, i) => ({ c, r: rank(c), i }))
    .filter((x) => x.r < 3)
    .sort((a, b) => a.r - b.r || a.i - b.i)
    .slice(0, limit)
    .map((x) => x.c);
}

/** Replaces the word the menu was for with `insert` and a space; returns the text and where the caret goes. */
export function applyChoice(text: string, ac: Autocomplete, insert: string): { text: string; caret: number } {
  const after = text.slice(ac.end);
  const glue = after.startsWith(" ") ? "" : " ";
  const next = text.slice(0, ac.start) + insert + glue + after;
  return { text: next, caret: ac.start + insert.length + 1 };
}

/** A path for `@`: quoted when it has spaces, so the agent reads it as one. */
export function mentionText(path: string): string {
  return /\s/.test(path) ? `@"${path}"` : `@${path}`;
}

/** Files matching `query` (the host already filtered them; this orders basename matches first). */
export function rankFiles(files: string[], query: string, limit = 50): string[] {
  const q = query.toLowerCase();
  if (!q) return files.slice(0, limit);
  const base = (f: string) => f.slice(f.replace(/\\/g, "/").lastIndexOf("/") + 1).toLowerCase();
  const rank = (f: string) => (base(f).startsWith(q) ? 0 : base(f).includes(q) ? 1 : 2);
  return files
    .map((f, i) => ({ f, r: rank(f), i }))
    .sort((a, b) => a.r - b.r || a.f.length - b.f.length || a.i - b.i)
    .slice(0, limit)
    .map((x) => x.f);
}

/** Moves the highlighted row of a menu with ↑ / ↓, wrapping around. */
export function moveActive(active: number, count: number, dir: -1 | 1): number {
  if (count <= 0) return 0;
  return (active + dir + count) % count;
}
