// Small helpers shared by the views.

import { useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";
import { t } from "../lib/i18n";

export const cx = (...parts: Array<string | false | null | undefined>) => parts.filter(Boolean).join(" ");

/** HH:MM:SS in local time. */
export function clock(ms: number, seconds = true) {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return seconds ? `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}` : `${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** "4:52" (or "4 分 52 秒" in Chinese) until `deadline`. */
export function countdown(deadline: number, now: number, zh: boolean) {
  const s = Math.max(0, Math.round((deadline - now) / 1000));
  const m = Math.floor(s / 60);
  const r = s % 60;
  return zh ? (m ? `${m} 分 ${r} 秒` : `${r} 秒`) : `${m}:${String(r).padStart(2, "0")}`;
}

/** How people know the agent: the MCP client's name made readable (`claude-code` → Claude Code). */
export function agentLabel(raw: string | null | undefined): string {
  if (!raw) return t("agent_unnamed");
  const known: Record<string, string> = {
    "claude-code": "Claude Code",
    "claude-ai": "Claude",
    claude: "Claude",
    cursor: "Cursor",
    "cursor-vscode": "Cursor",
    windsurf: "Windsurf",
    codex: "Codex",
    "codex-mcp-client": "Codex",
    goose: "Goose",
    cline: "Cline",
    zed: "Zed",
  };
  const key = raw.trim().toLowerCase();
  if (known[key]) return known[key];
  return raw
    .split(/[-_\s]+/)
    .filter(Boolean)
    .map((w) => w[0].toUpperCase() + w.slice(1))
    .join(" ");
}

/** The element's size, kept up to date. */
export function useSize<T extends HTMLElement>(): [RefObject<T | null>, { w: number; h: number }] {
  const ref = useRef<T | null>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setSize({ w: Math.round(e.contentRect.width), h: Math.round(e.contentRect.height) }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, size];
}

/** The current time, ticking every `ms`. */
export function useNow(ms = 1000) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), ms);
    return () => clearInterval(id);
  }, [ms]);
  return now;
}

/** The command a shell-like call runs, from its argument preview, else the preview itself. */
export function commandOf(argsPreview: string | null): string {
  if (!argsPreview) return "";
  try {
    const v = JSON.parse(argsPreview);
    if (typeof v.command === "string") return v.command;
    const { intent: _i, ...rest } = v;
    void _i;
    return JSON.stringify(rest, null, 1);
  } catch {
    return argsPreview;
  }
}

/** Commands that destroy or rewrite things get the red verb (risk is carried by words; red marks only this part). */
export function destructive(command: string): boolean {
  return /\b(Remove-Item|rm|rmdir|del|erase|rd|format|Format-Volume|Clear-Content|Stop-Computer|Restart-Computer|shutdown|reg\s+delete|Remove-|diskpart|mkfs|dd)\b/i.test(
    command,
  );
}
