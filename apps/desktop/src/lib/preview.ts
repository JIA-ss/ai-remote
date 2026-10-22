// Previews of files a coding agent made or changed (HTML, Markdown, images), as pure functions so
// they can be tested: which paths can be previewed, which text in a reply names one, and the
// document an HTML preview renders.
//
// The bytes come from the host's `chat_file` tool (see crates/ar-host/src/chat/preview.rs); the
// dialog is ui/chat/Preview.tsx.

import type { Block } from "./chat";

export type PreviewKind = "html" | "markdown" | "image";

const KINDS: Record<string, PreviewKind> = {
  html: "html",
  htm: "html",
  md: "markdown",
  markdown: "markdown",
  png: "image",
  jpg: "image",
  jpeg: "image",
  gif: "image",
  webp: "image",
  svg: "image",
  bmp: "image",
};

const MIME: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  svg: "image/svg+xml",
  bmp: "image/bmp",
};

/** Largest file the host lets a preview read (it refuses larger ones first). */
export const MAX_PREVIEW = 16 * 1024 * 1024;
/** Most text an HTML or Markdown preview renders; the rest is left out (the source view shows it). */
export const MAX_RENDERED = 2 * 1024 * 1024;

function extension(path: string): string {
  const name = path.split(/[\\/]/).pop() ?? "";
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

/** What `path` is previewed as, or null when it cannot be previewed. */
export function previewKind(path: string): PreviewKind | null {
  return KINDS[extension(path)] ?? null;
}

/** The MIME type of an image preview. */
export function imageMime(path: string): string {
  return MIME[extension(path)] ?? "application/octet-stream";
}

/**
 * The file a link or a bit of inline code in a reply names, when it names a previewable one:
 * `out/report.html`, `./notes.md`, `C:\work\chart.png`, `file:///tmp/a.svg`, `report.html#top`.
 * Web addresses, other schemes and text with spaces are not files.
 */
export function fileTarget(text: string): string | null {
  let s = text.trim();
  if (!s || s.length > 1024 || /\s/.test(s.replace(/%20/g, ""))) return null;
  if (/^file:\/\//i.test(s)) {
    s = s.replace(/^file:\/\//i, "");
    // file:///C:/x on Windows
    if (/^\/[A-Za-z]:\//.test(s)) s = s.slice(1);
    try {
      s = decodeURIComponent(s);
    } catch {
      return null;
    }
  } else if (/^[A-Za-z][A-Za-z0-9+.-]*:/.test(s) && !/^[A-Za-z]:[\\/]/.test(s)) {
    // http:, mailto:, data:, ... (but not a Windows drive)
    return null;
  }
  s = s.replace(/[#?].*$/, "").replace(/:\d+(:\d+)?$/, "");
  if (!s || s.endsWith("/") || s.endsWith("\\")) return null;
  return previewKind(s) ? s : null;
}

/**
 * The document an HTML preview renders, in a frame with `sandbox=""` (no scripts, an opaque origin,
 * no forms, no popups, no navigating the app). A policy at the top of its head adds that nothing
 * is fetched from the network (images only from `data:` and `blob:`, styles inline) and that links
 * open nowhere (a `_blank` target, which the sandbox refuses as a popup).
 */
export function previewDocument(source: string): string {
  const guard =
    `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: blob:; style-src 'unsafe-inline'; ` +
    `font-src data:; media-src data: blob:; base-uri 'none'; form-action 'none'"><base target="_blank">`;
  const head = /<head\b[^>]*>/i.exec(source);
  if (head) return source.slice(0, head.index + head[0].length) + guard + source.slice(head.index + head[0].length);
  const doctype = /^\s*<!doctype[^>]*>/i.exec(source);
  if (doctype) return doctype[0] + guard + source.slice(doctype[0].length);
  return guard + source;
}

/** Text of a file, cut to `max` bytes' worth (on a character boundary) when longer. */
export function decodeText(bytes: Uint8Array, max = MAX_RENDERED): { text: string; cut: boolean } {
  const cut = bytes.length > max;
  const text = new TextDecoder("utf-8", { fatal: false }).decode(cut ? bytes.subarray(0, max) : bytes);
  return { text: cut ? text.replace(/\uFFFD$/, "") : text, cut };
}

/** "12 KB", "3.4 MB". */
export function sizeLabel(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  return `${(n / 1048576).toFixed(1)} MB`;
}

/** Most files shown inline under one turn. */
export const MAX_INLINE = 4;

/**
 * The previewable files a finished turn made or named, in order, for the previews shown in the
 * conversation: files its tools wrote (an edit's diff, a location of a tool that is not a read or a
 * delete), then files its last reply names (a link or inline code). One per file name; failed tools
 * and deleted files are left out.
 */
export function turnFiles(blocks: Block[], max = MAX_INLINE): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  const add = (p: string | null) => {
    if (!p || !previewKind(p)) return;
    const name = (p.split(/[\\/]/).pop() ?? p).toLowerCase();
    if (seen.has(name)) return;
    seen.add(name);
    out.push(p);
  };
  for (const b of blocks) {
    if (b.kind !== "tool" || b.status === "failed" || b.tool === "read" || b.tool === "delete") continue;
    for (const c of b.content) if (c.type === "diff" && !(c.new === "" && c.old)) add(c.path);
    for (const l of b.locations) add(l);
  }
  const reply = [...blocks].reverse().find((b): b is Extract<Block, { kind: "text" }> => b.kind === "text");
  if (reply) {
    for (const m of reply.text.matchAll(/\]\(([^)\s]+)\)|`([^`\s]+)`/g)) add(fileTarget(m[1] ?? m[2]));
  }
  return out.slice(0, max);
}
