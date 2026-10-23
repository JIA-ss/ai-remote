// What can be done to one session from the sidebar's menu, the session page's menu and the session
// shortcuts (SB-26 / SB-35 / SB-39): pin, read state, Mark as done, archive and unarchive — each told
// in a toast, with an undo where there is one.

import { t } from "../../lib/i18n";
import type { WsItem } from "../../lib/workspace";
import type { Toast } from "./parts";
import { titleOf } from "./Sidebar";
import { workspace } from "./useWorkspace";

export interface ActionCtx {
  toast: (t: Omit<Toast, "id">) => void;
  /** A session left the list (archived): the page showing it moves on. */
  onArchived: (key: string) => void;
}

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

export function archive(x: WsItem, ctx: ActionCtx) {
  const known = x.meta;
  const title = titleOf(x);
  const fail = failure(ctx, x.host);
  void workspace.update(x.host, x.meta.id, { archived: true }).then(() => {
    ctx.onArchived(x.key);
    ctx.toast({ text: t("ws_archived", { title }), action: { label: t("ws_undo"), run: () => void workspace.update(x.host, known.id, { archived: false }, known).catch(fail) } });
  }, fail);
}

export function unarchive(x: WsItem, ctx: ActionCtx) {
  const title = titleOf(x);
  void workspace.update(x.host, x.meta.id, { archived: false }, x.meta).then(() => ctx.toast({ text: t("ws_unarchived", { title }) }), failure(ctx, x.host));
}
