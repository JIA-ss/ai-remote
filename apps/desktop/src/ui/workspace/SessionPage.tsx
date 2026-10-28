// A session in the workspace (clean-room spec SP): a 48 px title bar — the title (click to rename),
// then quieter "folder · computer · branch"; on the right, watching that computer live and a menu
// (pin, read state, archive, end) — over the conversation. Codex sessions use the Codex view; other
// sessions use the chat panel's conversation until the Claude view (ui/claude) takes their place.
// Whole-page states follow SP-41..45: not found, the host too old, no access, offline (reconnecting for
// 25 s first), still loading.

import { MonitorPlay, MoreHorizontal } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { folderName } from "../../lib/chat";
import { t } from "../../lib/i18n";
import type { Computer } from "../../lib/store";
import { sessionKey, type WsItem, type WsSnapshot } from "../../lib/workspace";
import type { ChatTarget } from "../AgentChat";
import { ClaudeConversation } from "../claude/ClaudeConversation";
import { CodexConversation, isCodexRuntime } from "../codex/CodexConversation";
import { Later, PopMenu, type MenuLine, type Toast } from "./parts";
import { RenameField, titleOf } from "./Sidebar";
import { archive, forkItem, markDone, readLocked, togglePin, toggleRead, unarchive, type ActionCtx } from "./actions";
import { workspace } from "./useWorkspace";

/** SP-45: how long a computer that dropped off counts as reconnecting before the page says it is offline. */
const RECONNECT_MS = 25_000;

export function SessionPage({
  snap,
  computers,
  host,
  id,
  ticket,
  corner,
  renaming,
  setRenaming,
  onWatch,
  onLeave,
  onArchived,
  onNew,
  onOpen,
  toast,
  embedded = false,
}: {
  snap: WsSnapshot;
  computers: Computer[];
  host: string;
  id: string;
  /** A ticket from `chat_start`, valid once (the conversation asks for a fresh one otherwise). */
  ticket: string | null;
  corner: ReactNode;
  renaming: boolean;
  setRenaming: (on: boolean) => void;
  onWatch: (host: string, session?: string) => void;
  onLeave: () => void;
  onArchived: (key: string) => void;
  onNew: () => void;
  /** Opens another session (a fork made from the menu). */
  onOpen?: (host: string, id: string) => void;
  toast: (t: Omit<Toast, "id">) => void;
  embedded?: boolean;
}) {
  const key = sessionKey(host, id);
  const item = useMemo(() => workspace.find(host, id), [snap, host, id]);
  const c = computers.find((x) => x.host.name === host) ?? null;
  const hostState = snap.hosts[host];
  const old = hostState?.update === false;
  const [menu, setMenu] = useState<DOMRect | null>(null);
  const [confirmEnd, setConfirmEnd] = useState(false);
  const opened = useRef(Date.now());
  // Once mounted, the conversation owns its reconnect / undelivered-message state. A transient
  // computer-list offline report must not unmount it and lose the draft, pending sends or Retry.
  const mountedConversation = useRef(false);
  const [now, setNow] = useState(Date.now());

  // This session is being looked at: what it says is read, now and while it goes on.
  useEffect(() => {
    workspace.setCurrent(key);
    return () => workspace.setCurrent(null);
  }, [key]);
  // Ask its computer now rather than at the next poll (a session started elsewhere, a fresh link).
  useEffect(() => {
    opened.current = Date.now();
    if (c?.online === true) void workspace.refresh(host);
  }, [host, id]);
  // The offline page changes from "reconnecting" to "offline" after RECONNECT_MS.
  useEffect(() => {
    if (c?.online !== false) return;
    const id = setTimeout(() => setNow(Date.now()), RECONNECT_MS + 50);
    return () => clearTimeout(id);
  }, [c?.online, c?.since]);

  const title = item ? titleOf(item) : "";
  const fail = (e: unknown) => {
    const msg = e instanceof Error ? e.message : String(e);
    toast({ text: msg === "unsupported" ? t("ws_old_host", { host }) : t("ws_failed", { error: msg }), tone: "error" });
  };
  const target: ChatTarget = useMemo(() => (ticket ? { session: id, ticket } : { session: id }), [id, ticket]);

  let body: ReactNode;
  const fetchedSince = hostState && hostState.status === "ok" && hostState.fetchedAt >= opened.current;
  if (c?.online === false && !mountedConversation.current) {
    // A dropped connection reconnects for a while; a computer with a reason (unreachable) is offline now.
    const since = c.since ?? 0;
    body =
      !c.problem && now - since < RECONNECT_MS && Date.now() - since < RECONNECT_MS ? (
        <PageState title={t("ws_reconnecting", { host })} />
      ) : (
        <PageState title={t("ws_disconnected", { host })} text={t("ws_disconnected_d")} />
      );
  } else if (hostState?.status === "unsupported") {
    body = <PageState title={t("ws_too_old", { host })} text={t("ws_too_old_d")} />;
  } else if (hostState?.status === "denied") {
    body = <PageState title={t("ws_denied", { host })} text={t("ws_denied_d")} />;
  } else if (!item && fetchedSince) {
    body = <PageState title={t("ws_not_found")} text={t("ws_not_found_d", { host })} action={{ label: t("ws_new_session"), run: onNew }} />;
  } else if (!item && hostState?.status === "error") {
    body = <PageState title={t("ws_load_failed")} text={hostState.error ?? undefined} action={{ label: t("ws_retry"), run: () => void workspace.refresh(host) }} />;
  } else if (!item) {
    body = (
      <Later>
        <PageState title={t("ws_loading")} />
      </Later>
    );
  } else {
    mountedConversation.current = true;
    const codex = isCodexRuntime({ id: item.meta.runtime, name: item.meta.name });
    body = (
      <div className="ws-convo">
        {codex ? (
          <CodexConversation key={key} host={host} target={target} onLeave={() => onLeave()} />
        ) : (
          <ClaudeConversation key={key} host={host} target={target} onLeave={() => onLeave()} variant="page" />
        )}
      </div>
    );
  }

  const menuItems = (x: WsItem): MenuLine[] => {
    const ctx: ActionCtx = { toast, onArchived, onOpen };
    const dot = workspace.dot(x);
    const unread = workspace.unread(x);
    if (x.meta.archived) {
      return [
        { key: "rename", label: t("ws_rename"), letter: "r", disabled: old, note: t("ws_old_host", { host }), run: () => setRenaming(true) },
        "sep",
        { key: "unarchive", label: t("ws_unarchive"), letter: "a", disabled: old, note: t("ws_old_host", { host }), run: () => unarchive(x, ctx) },
      ];
    }
    return [
      { key: "rename", label: t("ws_rename"), letter: "r", disabled: old, note: t("ws_old_host", { host }), run: () => setRenaming(true) },
      { key: "pin", label: t(workspace.isPinned(x) ? "ws_unpin" : "ws_pin"), letter: "p", run: () => togglePin(x, ctx) },
      dot === "failed"
        ? { key: "done", label: t("ws_mark_done"), letter: "u", run: () => markDone(x, ctx) }
        : { key: "read", label: t(unread ? "ws_mark_read" : "ws_mark_unread"), letter: "u", disabled: readLocked(x), note: t("ws_read_busy"), run: () => toggleRead(x) },
      forkItem(x, ctx),
      "sep",
      { key: "archive", label: t("ws_archive"), letter: "a", disabled: old, note: t("ws_old_host", { host }), run: () => archive(x, ctx) },
      ...(x.meta.state === "ended"
        ? []
        : [
            {
              key: "end",
              label: confirmEnd ? t("ws_end_confirm") : t("ws_end"),
              danger: true,
              keep: !confirmEnd,
              run: () => {
                if (!confirmEnd) setConfirmEnd(true);
                else {
                  setConfirmEnd(false);
                  void workspace.end(host, id).catch(fail);
                }
              },
            },
          ]),
    ];
  };

  const meta = item ? [folderName(item.meta.cwd), item.local ? null : host, item.meta.branch?.trim() || null].filter(Boolean).join(" · ") : host;
  return (
    <div className="ws-page ws-session">
      <header className="ws-tb" data-tauri-drag-region>
        {corner}
        <div className="ws-tb-lead">
          {item && renaming ? (
            <RenameField
              className="ws-title-edit"
              initial={item.meta.title?.trim() || title}
              onDone={(next) => {
                setRenaming(false);
                if (next !== null) void workspace.update(host, id, { title: next }).catch(fail);
              }}
            />
          ) : item ? (
            <TitleButton title={title} oldNote={old ? t("ws_old_host", { host }) : null} onRename={() => setRenaming(true)} />
          ) : null}
          <span className="ws-tb-meta" title={item?.meta.cwd}>
            {meta}
          </span>
        </div>
        <div className="ws-drag" data-tauri-drag-region />
        <div className="ws-tb-end">
          {!embedded && (
            <button type="button" className="ws-ib" disabled={c?.online !== true} onClick={() => onWatch(host, id)} aria-label={t("ws_open_stream", { host })} title={t("ws_open_stream", { host })}>
              <MonitorPlay size={16} strokeWidth={1.5} />
            </button>
          )}
          {item && (
            <button
              type="button"
              className="ws-ib"
              aria-haspopup="menu"
              aria-label={t("ws_session_menu", { title })}
              title={t("ws_more")}
              onClick={(e) => {
                setConfirmEnd(false);
                setMenu(e.currentTarget.getBoundingClientRect());
              }}
            >
              <MoreHorizontal size={16} strokeWidth={1.5} />
            </button>
          )}
        </div>
        <div className="ws-frame-room" />
      </header>
      <div className="ws-page-body">{body}</div>
      {menu && item && <PopMenu at={{ anchor: menu, side: "below" }} label={t("ws_session_menu", { title })} items={menuItems(item)} onClose={() => setMenu(null)} />}
    </div>
  );
}

/** The title as a button that renames (SP-04): the full title as its tooltip when it is cut, else "Rename". */
function TitleButton({ title, oldNote, onRename }: { title: string; oldNote: string | null; onRename: () => void }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [cut, setCut] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const check = () => setCut(el.scrollWidth > el.clientWidth + 1);
    check();
    const ro = new ResizeObserver(check);
    ro.observe(el);
    return () => ro.disconnect();
  }, [title]);
  return (
    <button
      ref={ref}
      type="button"
      className="ws-title"
      aria-disabled={oldNote ? true : undefined}
      aria-label={`${title}, ${t("ws_rename")}`}
      title={oldNote ?? (cut ? title : t("ws_rename"))}
      onClick={() => !oldNote && onRename()}
    >
      {title}
    </button>
  );
}

/** A whole-page state: a short title, a line of why, maybe one action (SP-41..45). */
function PageState({ title, text, action }: { title: string; text?: string; action?: { label: string; run: () => void } }) {
  return (
    <div className="ws-state">
      <div className="ws-state-box">
        <h2>{title}</h2>
        {text && <p>{text}</p>}
        {action && (
          <div className="ws-state-acts">
            <button type="button" className="ws-btn" onClick={action.run}>
              {action.label}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
