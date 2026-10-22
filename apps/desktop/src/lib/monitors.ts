// The host's monitors in the live view: what to call each one in the picker (where it sits next to
// the primary one, portrait or not), their order there, and the choice remembered per computer.
// Indexes are the host's (`system_info.monitors`, the live view's `monitor`); positions are its
// screen coordinates (physical pixels on Windows, points on macOS), so a monitor left of or above
// the primary one has a negative origin.

import type { Key } from "./i18n";

export interface RemoteMonitor {
  index: number;
  width: number;
  height: number;
  primary: boolean;
  /** Where it is in screen coordinates; hosts from before 2026-10-22 do not say. */
  x?: number;
  y?: number;
}

export type Side = "left" | "right" | "above" | "below";

const placed = (m: RemoteMonitor): m is RemoteMonitor & { x: number; y: number } =>
  typeof m.x === "number" && typeof m.y === "number";

/** Where `m` sits next to `primary`: clear of it on one axis, or else by the larger offset of their centres. */
export function sideOf(m: RemoteMonitor, primary: RemoteMonitor): Side | null {
  if (!placed(m) || !placed(primary)) return null;
  if (m.x + m.width <= primary.x) return "left";
  if (m.x >= primary.x + primary.width) return "right";
  if (m.y + m.height <= primary.y) return "above";
  if (m.y >= primary.y + primary.height) return "below";
  const dx = m.x + m.width / 2 - (primary.x + primary.width / 2);
  const dy = m.y + m.height / 2 - (primary.y + primary.height / 2);
  if (dx === 0 && dy === 0) return null;
  return Math.abs(dx) >= Math.abs(dy) ? (dx < 0 ? "left" : "right") : dy < 0 ? "above" : "below";
}

/** The primary monitor (the first when none is marked). */
export function primaryOf(list: RemoteMonitor[]): RemoteMonitor | undefined {
  return list.find((m) => m.primary) ?? list[0];
}

/** Left to right, then top to bottom, as they stand on the host's desk; host order without positions. */
export function pickerOrder(list: RemoteMonitor[]): RemoteMonitor[] {
  if (!list.every(placed)) return [...list].sort((a, b) => a.index - b.index);
  return [...list].sort((a, b) => a.x! - b.x! || a.y! - b.y! || a.index - b.index);
}

type T = (key: Key, vars?: Record<string, string | number>) => string;

/**
 * Each monitor's name in the picker, by index: "Main display", "Left portrait display", ...; a
 * number tells apart two on the same side, and "Display 3" is for a host that gives no positions.
 */
export function monitorNames(list: RemoteMonitor[], t: T): Map<number, string> {
  const primary = primaryOf(list);
  const names = new Map<number, string>();
  for (const m of pickerOrder(list)) {
    const side = m === primary ? null : primary && sideOf(m, primary);
    const kind = t(m.height > m.width ? "monitor_portrait" : "monitor_landscape");
    names.set(m.index, m === primary ? t("monitor_primary") : side ? t(`monitor_${side}`, { kind }) : t("monitor_n", { n: m.index + 1 }));
  }
  // Two on the same side: number them in picker order.
  const counts = new Map<string, number>();
  for (const name of names.values()) counts.set(name, (counts.get(name) ?? 0) + 1);
  const seen = new Map<string, number>();
  for (const m of pickerOrder(list)) {
    const name = names.get(m.index)!;
    if ((counts.get(name) ?? 0) < 2) continue;
    const n = (seen.get(name) ?? 0) + 1;
    seen.set(name, n);
    names.set(m.index, `${name} ${n}`);
  }
  return names;
}

/** A picker option: the name and the size, e.g. "Left portrait display · 1440×2560". */
export function monitorOption(m: RemoteMonitor, names: Map<number, string>): string {
  return `${names.get(m.index) ?? m.index + 1} · ${m.width}×${m.height}`;
}

/** Whether a refused start means the monitor asked for is gone (every host version's words). */
export function isMissingMonitor(reason: string): boolean {
  return /there is no monitor \d+|no such monitor|monitor not found/i.test(reason);
}

const KEY = "ai-remote.stream-monitor";

function choices(): Record<string, number> {
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return saved && typeof saved === "object" && !Array.isArray(saved) ? saved : {};
  } catch {
    return {};
  }
}

/** The monitor last picked for `host`, or null for the primary one. */
export function loadMonitor(host: string): number | null {
  const index = choices()[host];
  return Number.isInteger(index) && index >= 0 ? index : null;
}

/** Remembers the monitor picked for `host`; null forgets it (the primary one again). */
export function saveMonitor(host: string, index: number | null) {
  const all = choices();
  if (index === null) delete all[host];
  else all[host] = index;
  try {
    localStorage.setItem(KEY, JSON.stringify(all));
  } catch {
    // Storage full or blocked: the choice lasts for this page only.
  }
}
