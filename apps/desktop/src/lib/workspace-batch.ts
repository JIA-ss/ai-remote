// SB-34's supported subset: short-lived selection and scoped pin/read/archive batches. Nothing
// persists a selection, deletes history, changes a model, or treats partial success as all-or-none.
import { isRunning, type WorkspaceStore, type WsItem } from "./workspace";

export interface RowSelection { keys: string[]; anchor: string | null }
export const EMPTY_SELECTION: RowSelection = { keys: [], anchor: null };

export function selectRow(previous: RowSelection, key: string, order: string[], toggle: boolean, range: boolean): RowSelection {
  if (range) {
    const anchor = previous.anchor && order.includes(previous.anchor) ? previous.anchor : key;
    const a = order.indexOf(anchor), b = order.indexOf(key);
    if (a < 0 || b < 0) return previous;
    const keys = order.slice(Math.min(a, b), Math.max(a, b) + 1);
    return { keys: toggle ? [...new Set([...previous.keys, ...keys])] : keys, anchor };
  }
  if (!toggle) return EMPTY_SELECTION;
  const keys = previous.keys.includes(key) ? previous.keys.filter((k) => k !== key) : [...previous.keys, key];
  return { keys, anchor: keys.length ? key : null };
}

export type BatchKind = "pin" | "read" | "archive";
export type BatchChange =
  | { item: WsItem; field: "pinned" | "archived"; before: boolean; after: boolean }
  | { item: WsItem; field: "read"; before: ReturnType<WorkspaceStore["readMark"]>; after: ReturnType<WorkspaceStore["readMark"]> };
export interface BatchFailure { key: string; title: string; error: string }
export interface BatchResult { successes: BatchChange[]; failures: BatchFailure[] }
export const BATCH_CONCURRENCY = 8;

// Shared by forward actions and undo, including two simultaneously mounted callers.
let active = 0;
const waiting: Array<() => void> = [];
async function remote<T>(run: () => Promise<T>): Promise<T> {
  if (active >= BATCH_CONCURRENCY) await new Promise<void>((resolve) => waiting.push(resolve));
  else active++;
  try { return await run(); }
  finally { const next = waiting.shift(); if (next) next(); else active--; }
}

export const readLocked = (x: WsItem) => isRunning(x.meta) || !!x.meta.pending;
const failure = (item: WsItem, error: unknown): BatchFailure => ({ key: item.key, title: item.meta.title || item.meta.name, error: String((error as Error)?.message ?? error) });
const unique = (items: WsItem[]) => [...new Map(items.map((x) => [x.key, x])).values()];

export async function runBatch(store: WorkspaceStore, selected: WsItem[], kind: BatchKind, value: boolean): Promise<BatchResult> {
  const items = unique(selected);
  if (kind === "read") {
    const eligible = items.filter((x) => { const fresh = store.find(x.host, x.meta.id); return fresh && !readLocked(fresh); });
    const changes = new Map(store.changeRead(eligible.map((x) => x.key), value).map((change) => [change.key, change]));
    return {
      successes: items.flatMap((item): BatchChange[] => { const change = changes.get(item.key); return change ? [{ item, field: "read", before: change.before, after: change.after }] : []; }),
      failures: items.filter((x) => !changes.has(x.key)).map((x) => failure(x, store.find(x.host, x.meta.id) ? "busy_session" : "missing_session")),
    };
  }
  const field = kind === "pin" ? "pinned" : "archived";
  const outcomes = await Promise.all(items.map(async (item) => {
    try {
      const receipt = await remote(() => store.updateFlag(item.host, item.meta.id, field, value));
      return { success: { item: { ...item, meta: receipt.meta }, field, before: receipt.before, after: receipt.after } as BatchChange };
    } catch (error) { return { failure: failure(item, error) }; }
  }));
  return { successes: outcomes.flatMap((x) => x.success ? [x.success] : []), failures: outcomes.flatMap((x) => x.failure ? [x.failure] : []) };
}

/** Only successful, still-current changes can be undone; each remote request restores one field. */
export async function undoBatch(store: WorkspaceStore, changes: BatchChange[]): Promise<BatchResult> {
  const result: BatchResult = { successes: [], failures: [] };
  const local = changes.filter((c): c is Extract<BatchChange, { field: "read" }> => c.field === "read");
  const restored = new Set(store.restoreRead(local.map((c) => ({ key: c.item.key, before: c.before, after: c.after }))));
  for (const change of local) {
    if (restored.has(change.item.key)) result.successes.push({ ...change, before: change.after, after: change.before });
    else result.failures.push(failure(change.item, "state_changed"));
  }
  await Promise.all(changes.filter((c): c is Exclude<BatchChange, { field: "read" }> => c.field !== "read").map(async (change) => {
    try {
      const receipt = await remote(() => store.updateFlag(change.item.host, change.item.meta.id, change.field, change.before, change.item.meta, change.after));
      result.successes.push({ ...change, item: { ...change.item, meta: receipt.meta }, before: receipt.before, after: receipt.after });
    } catch (error) { result.failures.push(failure(change.item, error)); }
  }));
  return result;
}
