// What can be done to one session from the sidebar's menu, the session page's menu and the session
// shortcuts (SB-26 / SB-35 / SB-39): pin, read state, Mark as done, fork, archive and unarchive — each
// told in a toast, with an undo where there is one. Archiving a session that has forks asks first
// whether its forks go too (SB-37): the question is kept here and shown by `FamilyConfirm`.

import { t, type Key } from "../../lib/i18n";
import { forkBlock, forksOf, type ForkBlock, type WsItem } from "../../lib/workspace";
import type { MenuItem, Toast } from "./parts";
import { titleOf } from "./Sidebar";
import { workspace } from "./useWorkspace";

export interface ActionCtx {
  toast: (t: Omit<Toast, "id">) => void;
  /** A session left the list (archived): the page showing it moves on. */
  onArchived: (key: string) => void;
  /** Opens a session (a fork just made). */
  onOpen?: (host: string, id: string) => void;
}

const FORK_NOTE: Record<ForkBlock, Key> = {
  agent: "ws_fork_agent",
  no_turn: "ws_fork_no_turn",
  running: "ws_fork_running",
  archived: "ws_fork_archived",
};

/** The menus' "Fork" (letter f): off, with why, while the session cannot be forked. */
export function forkItem(x: WsItem, ctx: ActionCtx): MenuItem {
  const block = forkBlock(x.meta);
  return { key: "fork", label: t("ws_fork"), letter: "f", disabled: !!block, note: block ? t(FORK_NOTE[block]) : undefined, run: () => fork(x, ctx) };
}

/** Forks a session on its host and opens the fork (it is listed under its parent). */
export function fork(x: WsItem, ctx: ActionCtx) {
  if (forkBlock(x.meta)) return;
  void workspace.fork(x.host, x.meta.id, t("hc_intent")).then((r) => {
    ctx.toast({ text: t("ws_forked", { title: titleOf(x) }) });
    ctx.onOpen?.(x.host, r.session);
  }, failure(ctx, x.host));
}

/** A question waiting for an answer: archive a session alone, or with its forks. */
export interface FamilyAsk {
  item: WsItem;
  forks: WsItem[];
  ctx: ActionCtx;
}

let asking: FamilyAsk | null = null;
const askListeners = new Set<() => void>();

/** The archive question shown now (subscribe with `useSyncExternalStore`). */
export const familyAsk = {
  get: () => asking,
  subscribe: (l: () => void) => {
    askListeners.add(l);
    return () => {
      askListeners.delete(l);
    };
  },
  set(next: FamilyAsk | null) {
    asking = next;
    askListeners.forEach((l) => l());
  },
};

/** A refusal as a toast: an old host is named, anything else is quoted. */
export function failure(ctx: ActionCtx, host: string) {
  return (e: unknown) => {
    const msg = e instanceof Error ? e.message : String(e);
    ctx.toast({ text: msg === "unsupported" ? t("ws_old_host", { host }) : t("ws_failed", { error: msg }), tone: "error" });
  };
}

/** Read / unread can only change while the session neither runs nor waits for someone (SB-25). */
export const readLocked = (x: WsItem) => {
  const dot = workspace.dot(x);
  return dot === "running" || dot === "awaiting";
};

export function togglePin(x: WsItem, ctx: ActionCtx) {
  void workspace.update(x.host, x.meta.id, { pinned: !workspace.isPinned(x) }).catch(failure(ctx, x.host));
}

export function toggleRead(x: WsItem) {
  if (readLocked(x)) return;
  if (workspace.unread(x)) workspace.markRead(x.key);
  else workspace.markUnread(x.key);
}

/** SB-26: a failed session put aside; the toast can bring it back as it was. */
export function markDone(x: WsItem, ctx: ActionCtx) {
  const undo = workspace.markDone(x.key);
  ctx.toast({ text: t("ws_marked_done", { title: titleOf(x) }), action: { label: t("ws_undo"), run: undo } });
}

/** Archives a session; one with forks not archived asks first (SB-37). */
export function archive(x: WsItem, ctx: ActionCtx) {
  const forks = forksOf(x, workspace.items()).filter((f) => !f.meta.archived);
  if (forks.length) familyAsk.set({ item: x, forks, ctx });
  else archiveOne(x, ctx);
}

function archiveOne(x: WsItem, ctx: ActionCtx, undone?: () => void) {
  const known = x.meta;
  const title = titleOf(x);
  const fail = failure(ctx, x.host);
  void workspace.update(x.host, x.meta.id, { archived: true }).then(() => {
    ctx.onArchived(x.key);
    ctx.toast({
      text: t("ws_archived", { title }),
      action: {
        label: t("ws_undo"),
        run: () => {
          undone?.();
          void workspace.update(x.host, known.id, { archived: false }, known).catch(fail);
        },
      },
    });
  }, fail);
}

/**
 * The answer to the archive question: only the session (its own forks move to the top level, on this
 * device), or the session and every fork under it. Either can be undone from the toast.
 */
export function archiveFamily(ask: FamilyAsk, all: boolean) {
  familyAsk.set(null);
  const { item: x, forks, ctx } = ask;
  if (!all) {
    const own = forks.filter((f) => f.host === x.host && f.meta.parent === x.meta.id).map((f) => f.key);
    workspace.detach(own, true);
    archiveOne(x, ctx, () => workspace.detach(own, false));
    return;
  }
  const targets = [x, ...forks];
  void Promise.allSettled(targets.map((y) => workspace.update(y.host, y.meta.id, { archived: true }))).then((results) => {
    const done = targets.filter((_, i) => results[i].status === "fulfilled");
    const failed = results.find((r): r is PromiseRejectedResult => r.status === "rejected");
    for (const y of done) ctx.onArchived(y.key);
    const why = failed ? ` — ${t("ws_failed", { error: String((failed.reason as Error)?.message ?? failed.reason) })}` : "";
    ctx.toast({
      text: t("ws_archived_n", { n: done.length }) + why,
      tone: failed ? "error" : undefined,
      action: done.length
        ? { label: t("ws_undo"), run: () => done.forEach((y) => void workspace.update(y.host, y.meta.id, { archived: false }, y.meta).catch(failure(ctx, y.host))) }
        : undefined,
    });
  });
}

export function unarchive(x: WsItem, ctx: ActionCtx) {
  const title = titleOf(x);
  void workspace.update(x.host, x.meta.id, { archived: false }, x.meta).then(() => ctx.toast({ text: t("ws_unarchived", { title }) }), failure(ctx, x.host));
}
