// The workspace's sidebar (clean-room spec SB, flat variant): the 48 px strip with the window corner and
// the computers / agents switch, a search field that opens the ⌘K panel, "New", then the sessions —
// pinned ones first, then one group per folder on a computer (five rows each, "Show N more") — and a
// footer with this device and Settings. Rows are one line: the title fading out at its end, the branch,
// and the status dot that turns into "…" on hover. A right click or "…" opens the row's menu.

import { ChevronRight, CodeXml, GitBranch, MoreHorizontal, Plus, Search, Settings, SquarePen } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { knownHome } from "../../lib/chat";
import { t } from "../../lib/i18n";
import type { Computer } from "../../lib/store";
import {
  aggregateDot,
  GROUP_ROWS,
  groupSessions,
  truncateGroup,
  type Place,
  type WsItem,
  type WsSnapshot,
} from "../../lib/workspace";
import { cx } from "../util";
import { ModeSwitch, type AppMode } from "./ModeSwitch";
import { ComputerMark, modLabel, PopMenu, StatusDot, useOverflow, type MenuItem, type Toast } from "./parts";
import { workspace } from "./useWorkspace";

const COLLAPSED = "ai-remote.ws.collapsed";
const PINNED_KEY = "pinned";

function readCollapsed(): Set<string> {
  try {
    return new Set(JSON.parse(localStorage.getItem(COLLAPSED) ?? "[]") as string[]);
  } catch {
    return new Set();
  }
}

/** A session's title as shown ("Untitled session" when it has none at all). */
export function titleOf(x: WsItem): string {
  return workspace.title(x) || t("ws_untitled");
}

export interface SidebarProps {
  snap: WsSnapshot;
  computers: Computer[];
  corner: ReactNode;
  mode: AppMode;
  onMode: (m: AppMode) => void;
  /** The open session (`host/id`), or null on the new-session page. */
  current: string | null;
  onOpen: (host: string, id: string) => void;
  onNew: (place?: Place) => void;
  onPalette: () => void;
  onPair: () => void;
  onSettings: () => void;
  onArchived: (key: string) => void;
  device: string;
  toast: (t: Omit<Toast, "id">) => void;
  /** A row to rename now (F2 / ⌘⌥R from elsewhere), by key. */
  renaming: string | null;
  setRenaming: (key: string | null) => void;
}

export function Sidebar(p: SidebarProps) {
  const items = useMemo(() => workspace.items(), [p.snap]);
  const { pinned, groups } = useMemo(
    () => groupSessions(items, { pinned: (x) => workspace.isPinned(x), title: titleOf, home: knownHome }),
    [items, p.snap.pins],
  );
  const [collapsed, setCollapsed] = useState(readCollapsed);
  const [opened, setOpened] = useState<Set<string>>(new Set());
  const [menu, setMenu] = useState<{ key: string; at: { x: number; y: number } } | null>(null);
  const [focusId, setFocusId] = useState<string | null>(null);
  const list = useRef<HTMLDivElement>(null);
  const byKey = useMemo(() => new Map(items.map((x) => [x.key, x])), [items]);
  const computer = (host: string) => p.computers.find((c) => c.host.name === host) ?? null;

  const toggleGroup = (key: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      try {
        localStorage.setItem(COLLAPSED, JSON.stringify([...next]));
      } catch {
        // Kept for this run only.
      }
      return next;
    });
  };

  // The open session's group opens when it is folded (SB-18), so the row is in view.
  useEffect(() => {
    if (!p.current) return;
    const el = list.current?.querySelector<HTMLElement>(`[data-row-key="${CSS.escape(p.current)}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [p.current]);

  // Roving focus (SB-40): ↑ / ↓ over rows and group headers, Home / End, F2 renames.
  const onListKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.target instanceof HTMLInputElement) return;
    const nav = [...(list.current?.querySelectorAll<HTMLElement>("[data-ws-nav]") ?? [])];
    const i = nav.indexOf(document.activeElement as HTMLElement);
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    let next = -1;
    if (e.key === "ArrowDown") next = (i + 1) % nav.length;
    else if (e.key === "ArrowUp") next = (i - 1 + nav.length) % nav.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = nav.length - 1;
    else if (e.key === "F2") {
      const key = (document.activeElement as HTMLElement | null)?.closest<HTMLElement>("[data-row-key]")?.dataset.rowKey;
      if (key) {
        e.preventDefault();
        p.setRenaming(key);
      }
      return;
    }
    if (next < 0 || !nav.length) return;
    e.preventDefault();
    nav[next].focus();
    nav[next].scrollIntoView({ block: "nearest" });
  };
  // One Tab stop for the list: the last focused item, else the first header.
  const navIds = useMemo(() => {
    const ids = new Set<string>();
    if (pinned.length) ids.add(`group:${PINNED_KEY}`);
    for (const g of groups) ids.add(`group:${g.key}`).add(`more:${g.key}`);
    for (const x of items) ids.add(`row:${x.key}`);
    return ids;
  }, [items, pinned, groups]);
  const firstNav = pinned.length ? `group:${PINNED_KEY}` : groups[0] ? `group:${groups[0].key}` : null;
  const stop = focusId && navIds.has(focusId) ? focusId : firstNav;
  const navIndex = (id: string) => (id === stop ? 0 : -1);

  const menuItems = (x: WsItem): Array<MenuItem | "sep"> => {
    const host = p.snap.hosts[x.host];
    const old = host?.update === false;
    const oldNote = t("ws_old_host", { host: x.host });
    const isPinned = workspace.isPinned(x);
    const dot = workspace.dot(x);
    const busy = dot === "running" || dot === "awaiting";
    const unread = workspace.unread(x);
    const fail = (e: unknown) => {
      const msg = e instanceof Error ? e.message : String(e);
      p.toast({ text: msg === "unsupported" ? oldNote : t("ws_failed", { error: msg }), tone: "error" });
    };
    return [
      { key: "rename", label: t("ws_rename"), letter: "r", disabled: old, note: oldNote, run: () => p.setRenaming(x.key) },
      { key: "pin", label: t(isPinned ? "ws_unpin" : "ws_pin"), letter: "p", run: () => void workspace.update(x.host, x.meta.id, { pinned: !isPinned }).catch(fail) },
      {
        key: "read",
        label: t(unread ? "ws_mark_read" : "ws_mark_unread"),
        letter: "u",
        disabled: busy,
        note: t("ws_read_busy"),
        run: () => (unread ? workspace.markRead(x.key) : workspace.markUnread(x.key)),
      },
      "sep",
      {
        key: "archive",
        label: t("ws_archive"),
        letter: "a",
        disabled: old,
        note: oldNote,
        run: () => {
          const known = x.meta;
          const title = titleOf(x);
          void workspace.update(x.host, x.meta.id, { archived: true }).then(() => {
            p.onArchived(x.key);
            p.toast({ text: t("ws_archived", { title }), action: { label: t("ws_undo"), run: () => void workspace.update(x.host, known.id, { archived: false }, known).catch(fail) } });
          }, fail);
        },
      },
      ...(x.meta.state === "ended" ? [] : [endItem(x, fail)]),
    ];
  };
  const [confirmEnd, setConfirmEnd] = useState<string | null>(null);
  const endItem = (x: WsItem, fail: (e: unknown) => void): MenuItem => ({
    key: "end",
    label: confirmEnd === x.key ? t("ws_end_confirm") : t("ws_end"),
    danger: true,
    keep: confirmEnd !== x.key,
    run: () => {
      if (confirmEnd !== x.key) setConfirmEnd(x.key);
      else {
        setConfirmEnd(null);
        void workspace.end(x.host, x.meta.id).catch(fail);
      }
    },
  });
  const menuOf = menu ? byKey.get(menu.key) : null;

  const row = (x: WsItem, opts: { lead?: boolean } = {}) => (
    <Row
      key={x.key}
      item={x}
      lead={opts.lead}
      selected={x.key === p.current}
      menuOpen={menu?.key === x.key}
      renaming={p.renaming === x.key}
      tabIndex={navIndex(`row:${x.key}`)}
      onFocus={() => setFocusId(`row:${x.key}`)}
      onOpen={() => p.onOpen(x.host, x.meta.id)}
      onMenu={(at) => {
        setConfirmEnd(null);
        setMenu({ key: x.key, at });
      }}
      onRenamed={(title) => {
        p.setRenaming(null);
        if (title === null) return;
        void workspace.update(x.host, x.meta.id, { title }).catch((e) => {
          const msg = e instanceof Error ? e.message : String(e);
          p.toast({ text: msg === "unsupported" ? t("ws_old_host", { host: x.host }) : t("ws_failed", { error: msg }), tone: "error" });
        });
      }}
    />
  );

  const noComputers = p.computers.length === 0;
  const loading = !items.length && p.computers.some((c) => c.online === true && (!p.snap.hosts[c.host.name] || p.snap.hosts[c.host.name].status === "loading"));
  const notes = p.computers
    .map((c) => ({ c, h: p.snap.hosts[c.host.name] }))
    .filter(({ c, h }) => c.online === true && h && (h.status === "denied" || h.status === "unsupported" || (h.status === "error" && !h.sessions.length)));

  return (
    <nav className="ws-side" aria-label={t("ws_sidebar")}>
      <div className="ws-side-top" data-tauri-drag-region>
        {p.corner}
        <div className="ws-drag" data-tauri-drag-region />
        <ModeSwitch mode={p.mode} onMode={p.onMode} />
      </div>
      <button type="button" className="ws-search" onMouseDown={(e) => e.preventDefault()} onClick={p.onPalette}>
        <span className="ws-lead">
          <Search size={15} strokeWidth={1.5} />
        </span>
        <span className="ws-search-label">{t("ws_search")}</span>
        <span className="ws-search-keys">{modLabel}K</span>
      </button>
      <button type="button" className={cx("ws-nav", p.current === null && "on")} aria-current={p.current === null ? "page" : undefined} onClick={() => p.onNew()}>
        <span className="ws-lead">
          <SquarePen size={15} strokeWidth={1.5} />
        </span>
        <span>{t("ws_new")}</span>
      </button>
      <div className="ws-list" ref={list} onKeyDown={onListKey} role="list" aria-label={t("ws_sessions")}>
        {noComputers ? (
          <div className="ws-list-empty">
            <p>{t("ws_no_computers")}</p>
            <button type="button" className="ws-btn" onClick={p.onPair}>
              {t("ws_pair")}
            </button>
          </div>
        ) : loading ? (
          <div className="ws-skel" aria-label={t("ws_loading")}>
            <i style={{ width: "75%" }} />
            <i style={{ width: "50%" }} />
          </div>
        ) : !items.length && !notes.length ? (
          <div className="ws-list-note">{t("ws_empty")}</div>
        ) : null}
        {pinned.length > 0 && (
          <Section
            id={PINNED_KEY}
            label={t("ws_pinned")}
            collapsed={collapsed.has(PINNED_KEY)}
            onToggle={() => toggleGroup(PINNED_KEY)}
            dot={aggregateDot(pinned.map((x) => workspace.dot(x)))}
            tabIndex={navIndex(`group:${PINNED_KEY}`)}
            onFocus={() => setFocusId(`group:${PINNED_KEY}`)}
          >
            {(collapsed.has(PINNED_KEY) ? pinned.filter((x) => x.key === p.current) : pinned).map((x) => row(x, { lead: true }))}
          </Section>
        )}
        {groups.map((g) => {
          const folded = collapsed.has(g.key);
          const keep = new Set(p.current ? [p.current] : []);
          const cut = truncateGroup(g.items, GROUP_ROWS, keep, opened.has(g.key), (x) => workspace.dot(x) === "awaiting");
          const c = computer(g.host);
          const offline = c?.online !== true;
          return (
            <Section
              key={g.key}
              id={g.key}
              label={g.label}
              title={g.local ? g.folder : t("ws_group_where", { host: g.host, path: g.folder })}
              collapsed={folded}
              onToggle={() => toggleGroup(g.key)}
              dot={aggregateDot(g.items.map((x) => workspace.dot(x)))}
              tabIndex={navIndex(`group:${g.key}`)}
              onFocus={() => setFocusId(`group:${g.key}`)}
              trailing={
                <>
                  {!g.local && (
                    <span className="ws-gh-env" title={t("ws_group_where", { host: g.host, path: g.folder })}>
                      <ComputerMark os={c?.os ?? null} online={c?.online ?? null} />
                    </span>
                  )}
                  <button
                    type="button"
                    className="ws-gh-add"
                    tabIndex={-1}
                    disabled={offline}
                    aria-label={offline ? t("ws_new_offline", { host: g.host }) : t("ws_new_in", { folder: g.label })}
                    title={offline ? t("ws_new_offline", { host: g.host }) : t("ws_new_in", { folder: g.label })}
                    onClick={() => p.onNew({ host: g.host, cwd: g.folder })}
                  >
                    <Plus size={15} strokeWidth={1.5} />
                  </button>
                </>
              }
            >
              {folded ? g.items.filter((x) => x.key === p.current).map((x) => row(x)) : cut.shown.map((x) => row(x))}
              {!folded && (cut.hidden.length > 0 || opened.has(g.key)) && g.items.length > GROUP_ROWS && (
                <button
                  type="button"
                  className="ws-more"
                  data-ws-nav
                  tabIndex={navIndex(`more:${g.key}`)}
                  onFocus={() => setFocusId(`more:${g.key}`)}
                  onClick={() =>
                    setOpened((prev) => {
                      const next = new Set(prev);
                      if (next.has(g.key)) next.delete(g.key);
                      else next.add(g.key);
                      return next;
                    })
                  }
                >
                  <span>{opened.has(g.key) ? t("ws_show_less") : t(cut.hidden.length === 1 ? "ws_show_more_one" : "ws_show_more_many", { n: cut.hidden.length })}</span>
                  {cut.hiddenNeedsYou && !opened.has(g.key) && <StatusDot kind="awaiting" />}
                </button>
              )}
            </Section>
          );
        })}
        {notes.length > 0 && (
          <div className="ws-host-notes">
            {notes.map(({ c, h }) => (
              <div key={c.host.name} className="ws-host-note">
                {h.status === "denied"
                  ? t("ws_host_denied", { host: c.host.name })
                  : h.status === "unsupported"
                    ? t("ws_host_old", { host: c.host.name })
                    : t("ws_host_error", { host: c.host.name, error: h.error ?? "" })}
              </div>
            ))}
          </div>
        )}
      </div>
      <div className="ws-foot">
        <span className="ws-foot-me" title={t("ws_this_device")}>
          <ComputerMark os={null} online={true} />
          <span className="ws-foot-name">{p.device}</span>
        </span>
        <button type="button" className="ws-ib" onClick={p.onSettings} aria-label={t("ws_settings")} title={t("ws_settings")}>
          <Settings size={15} strokeWidth={1.5} />
        </button>
      </div>
      {menu && menuOf && (
        <PopMenu at={menu.at} label={t("ws_session_menu", { title: titleOf(menuOf) })} items={menuItems(menuOf)} onClose={() => setMenu(null)} />
      )}
    </nav>
  );
}

/** A section: its header (a fold button, the badge and trailing controls) and its rows (SB-16 / SB-18). */
function Section({
  id,
  label,
  title,
  collapsed,
  onToggle,
  dot,
  trailing,
  tabIndex,
  onFocus,
  children,
}: {
  id: string;
  label: string;
  title?: string;
  collapsed: boolean;
  onToggle: () => void;
  dot: ReturnType<typeof aggregateDot>;
  trailing?: ReactNode;
  tabIndex?: number;
  onFocus: () => void;
  children: ReactNode;
}) {
  const ref = useOverflow<HTMLSpanElement>(label);
  return (
    <div className="ws-sec" data-collapsed={collapsed ? "" : undefined} data-group={id}>
      <div className="ws-gh">
        <button type="button" className="ws-gh-toggle" data-ws-nav data-group-toggle aria-expanded={!collapsed} title={title} tabIndex={tabIndex} onFocus={onFocus} onClick={onToggle}>
          <span className="ws-gh-label" ref={ref}>
            {label}
          </span>
          <ChevronRight className="ws-gh-caret" size={12} strokeWidth={2} />
        </button>
        <span className="ws-gh-badge">{collapsed && dot && <StatusDot kind={dot} />}</span>
        {trailing}
      </div>
      <div className="ws-sec-rows" role="group" aria-label={label}>
        {children}
      </div>
    </div>
  );
}

function Row({
  item,
  lead,
  selected,
  menuOpen,
  renaming,
  tabIndex,
  onFocus,
  onOpen,
  onMenu,
  onRenamed,
}: {
  item: WsItem;
  lead?: boolean;
  selected: boolean;
  menuOpen: boolean;
  renaming: boolean;
  tabIndex?: number;
  onFocus: () => void;
  onOpen: () => void;
  onMenu: (at: { x: number; y: number }) => void;
  onRenamed: (title: string | null) => void;
}) {
  const title = titleOf(item);
  const dot = workspace.dot(item);
  const ref = useOverflow<HTMLSpanElement>(title);
  const branch = item.meta.branch?.trim();
  const decorated = !!dot || !!branch;
  return (
    <div
      className="ws-row"
      role="listitem"
      data-row-key={item.key}
      data-selected={selected ? "" : undefined}
      data-menu-open={menuOpen ? "" : undefined}
      data-decorated={decorated ? "" : undefined}
      onContextMenu={(e) => {
        e.preventDefault();
        onMenu({ x: e.clientX, y: e.clientY });
      }}
    >
      {renaming ? (
        <RenameField initial={item.meta.title?.trim() || title} onDone={onRenamed} />
      ) : (
        <button type="button" className="ws-row-main" data-ws-nav tabIndex={tabIndex} onFocus={onFocus} onClick={onOpen} aria-current={selected ? "page" : undefined}>
          {lead && (
            <span className="ws-row-lead">
              <CodeXml size={14} strokeWidth={1.5} />
            </span>
          )}
          <span className="ws-row-title" ref={ref}>
            {title}
          </span>
        </button>
      )}
      {!renaming && branch && (
        <span className="ws-row-branch" title={t("ws_branch", { branch })}>
          <GitBranch size={12} strokeWidth={1.5} />
          <span>{branch}</span>
        </span>
      )}
      {!renaming && (
        <span className="ws-row-end">
          {dot && <StatusDot kind={dot} className="ws-row-dot" />}
          <button
            type="button"
            className="ws-row-more"
            tabIndex={-1}
            aria-label={t("ws_session_menu", { title })}
            title={t("ws_more")}
            onClick={(e) => {
              const r = e.currentTarget.getBoundingClientRect();
              onMenu({ x: r.left, y: r.bottom + 4 });
            }}
          >
            <MoreHorizontal size={15} strokeWidth={1.5} />
          </button>
        </span>
      )}
    </div>
  );
}

/**
 * Renaming in place (SB-22): all selected; Enter or leaving the field keeps it, Esc does not; the
 * trimmed text, empty going back to the automatic title, and nothing when it did not change. A blur
 * right after it opened (250 ms) takes the focus back.
 */
export function RenameField({ initial, onDone, className }: { initial: string; onDone: (title: string | null) => void; className?: string }) {
  const ref = useRef<HTMLInputElement>(null);
  const opened = useRef(Date.now());
  const done = useRef(false);
  const [value, setValue] = useState(initial);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
    if (ref.current) ref.current.scrollLeft = 0;
  }, []);
  const finish = (keep: boolean) => {
    if (done.current) return;
    done.current = true;
    const v = value.trim();
    onDone(keep && v !== initial.trim() ? v : null);
  };
  return (
    <input
      ref={ref}
      className={cx("ws-rename", className)}
      aria-label={t("ws_rename_field")}
      value={value}
      size={Math.min(Math.max(value.length, 1), 30)}
      spellCheck={false}
      onChange={(e) => setValue(e.target.value)}
      onKeyDown={(e) => {
        if (e.nativeEvent.isComposing || e.repeat) return;
        if (e.key === "Enter") {
          e.preventDefault();
          finish(true);
        } else if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          finish(false);
        }
      }}
      onBlur={() => {
        if (Date.now() - opened.current < 250) {
          requestAnimationFrame(() => ref.current?.focus());
          return;
        }
        finish(true);
      }}
    />
  );
}
