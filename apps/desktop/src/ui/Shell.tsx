// Window chrome and navigation. The shell (App.tsx) keeps one window: the sidebar flush with the left
// edge, expanded (240 px) or compact (72 px, icons only), and the content column with a transparent
// toolbar strip of glass capsules: ‹ › history, the page title (the page's only h1), a centre slot
// (tabs, settings sections), actions, and at most one tinted primary action. On macOS the traffic
// lights sit over the sidebar's top strip; on Windows the window frame draws the caption buttons.
// The sidebar's top carries the computers / agents switch (ui/workspace/ModeSwitch.tsx): "agents"
// hands the whole window to the agents workspace, a route of this shell (App.tsx).
//
// TitleBar stays backward compatible: the session page still passes title / facts / onBack / end and
// gets the shell's history through ShellContext; without a shell (tests) it draws only what it gets.

import { createContext, useContext, useRef, useState, type ReactNode, type RefObject } from "react";
import { api } from "../lib/api";
import { t } from "../lib/i18n";
import type { NavKey, SidebarState } from "../lib/nav";
import { detectOs } from "../lib/platform";
import { shortcutLabel } from "../lib/shortcuts";
import { groupOf, type Computer, type Group } from "../lib/store";
import { Glyph, Mark, OsGlyph } from "./glyphs";
import { Icon } from "./Icon";
import { TurnLine, turnSpec } from "./TurnLine";
import { clock, cx, routeLabel } from "./util";
import { ModeSwitch, type AppMode } from "./workspace/ModeSwitch";

/** A sidebar row (the session page names it so). */
export type Nav = NavKey;

/** What the shell offers the pages inside it (TitleBar reads it; the session page gets it this way). */
export interface ShellApi {
  sidebar: SidebarState;
  toggleSidebar: () => void;
  back: () => void;
  forward: () => void;
  canBack: boolean;
  canForward: boolean;
  /** Requests waiting for you anywhere. */
  attention: number;
}

export const ShellContext = createContext<ShellApi | null>(null);

const os = detectOs();

export function TitleBar({
  title,
  subtitle,
  facts,
  onBack,
  center,
  end,
  primary,
  bare,
  srTitle,
  lead,
}: {
  title?: string;
  /** A second line under the title (falls back to `facts`). */
  subtitle?: ReactNode;
  facts?: ReactNode;
  /** Overrides the shell's back (the session passes the shell's own). */
  onBack?: () => void;
  /** Tabs or a segmented control, centred in the free space. */
  center?: ReactNode;
  /** Actions, grouped in one capsule. */
  end?: ReactNode;
  /** The page's one tinted action. */
  primary?: ReactNode;
  /** No history buttons and no title (first run). */
  bare?: boolean;
  /** A heading for screen readers when the visible title is the centre control (settings). */
  srTitle?: string;
  /** Something before the title (the computer's status dot). */
  lead?: ReactNode;
}) {
  const shell = useContext(ShellContext);
  const back = onBack ?? shell?.back;
  const sub = subtitle ?? facts;
  return (
    <header className={cx("tb", "gl-toolbar", bare && "bare")} data-tauri-drag-region>
      {!bare &&
        (shell ? (
          <div className="gl-cap gl-glass thin gl-nav" role="group" aria-label={t("sc_back_fwd")}>
            <button
              type="button"
              className="back"
              onClick={back}
              disabled={!onBack && !shell.canBack}
              aria-label={t("back")}
              title={`${t("back")} · ${shortcutLabel("back", os)}`}
            >
              <Icon name="chevL" size={16} />
            </button>
            <button
              type="button"
              className="gl-fwd"
              onClick={shell.forward}
              disabled={!shell.canForward}
              aria-label={t("nav_forward")}
              title={`${t("nav_forward")} · ${shortcutLabel("forward", os)}`}
            >
              <Icon name="chevR" size={16} />
            </button>
          </div>
        ) : (
          onBack && (
            <button type="button" className="back" onClick={onBack} aria-label={t("back")}>
              <Icon name="back" size={14} />
            </button>
          )
        ))}
      {!bare && title && (
        <div className={cx("gl-titles", !!sub && "two")} data-tauri-drag-region>
          <h1 className="title" data-tauri-drag-region>
            {lead}
            <span className="tx">{title}</span>
          </h1>
          {sub && <div className="gl-sub facts">{sub}</div>}
        </div>
      )}
      {srTitle && <h1 className="gl-sr">{srTitle}</h1>}
      {center && <div className="gl-center">{center}</div>}
      <div className="drag" data-tauri-drag-region />
      {api.kind === "demo" && <span className="demo-badge">{t("preview_badge")}</span>}
      {end && <div className="gl-cap gl-glass thin nt-tb-end">{end}</div>}
      {primary && <div className="gl-primary">{primary}</div>}
      {/* Room for the caption buttons drawn by the window frame (Windows). */}
      <div className="gl-caption-space" />
    </header>
  );
}

/** One line of facts with glyphs, never chips. */
export function Facts({ items }: { items: Array<[ReactNode, string]> }) {
  return (
    <>
      {items.map(([g, text], i) => (
        <span key={i}>
          {g}
          {text}
        </span>
      ))}
    </>
  );
}

const levelGlyph = { observe: "level1", control: "level2", admin: "level3" } as const;
const levelName = { observe: "lvl_observe", control: "lvl_control", admin: "lvl_admin" } as const;

export function computerFacts(c: Computer): Array<[ReactNode, string]> {
  const out: Array<[ReactNode, string]> = [];
  if (c.os) out.push([<OsGlyph key="os" os={c.os} size={12} />, t(c.os === "mac" ? "mac" : "win")]);
  const lvl = c.feed.level !== undefined ? c.feed.level : c.host.levelAtPairing;
  if (lvl) out.push([<Glyph key="lvl" name={levelGlyph[lvl]} size={12} />, t(levelName[lvl])]);
  if (c.feed.bypass) out.push([<Glyph key="bypass" name="approval" size={12} />, t("bypass_fact")]);
  if (c.online === true && (c.host.path === "direct" || c.host.path === "relay"))
    out.push([<Glyph key="p" name={c.host.path === "relay" ? "relayed" : "direct"} size={12} />, routeLabel(c.host)]);
  return out;
}

/** The status sentence of a computer row and its tone. */
export function statusOf(c: Computer, now: number): { text: string; tone: "" | "you" | "agent" } {
  if (c.online === null) return { text: t("st_connecting"), tone: "" };
  const g = groupOf(c, now);
  if (g === "offline") return { text: c.since ? t("st_offline_since", { time: clock(c.since, false) }) : t("st_offline"), tone: "" };
  if (g === "needs") return { text: t("st_waiting"), tone: "you" };
  if (g === "you") return { text: t("st_you"), tone: "you" };
  if (c.feed.driver.state === "paused") return { text: t("st_paused"), tone: "" };
  if (g === "working") {
    const last = c.feed.steps.at(-1);
    return { text: last?.intent ?? t("st_running", { tool: last?.tool ?? "" }), tone: "agent" };
  }
  return { text: c.since ? t("st_idle_since", { time: clock(c.since, false) }) : t("st_idle"), tone: "" };
}

function PcRow({ c, sel, now, onClick, width }: { c: Computer; sel: boolean; now: number; onClick: () => void; width: number }) {
  const st = statusOf(c, now);
  const spec = c.online === true && (Object.keys(c.feed.sessions).length || c.feed.pending.length) ? turnSpec(c.feed, now, true) : null;
  const off = c.online === false;
  return (
    <button
      type="button"
      className={cx("pc", sel && "sel", off && "off", st.tone && `is-${st.tone}`)}
      title={`${c.host.name} · ${st.text}`}
      onClick={onClick}
      aria-current={sel ? "page" : undefined}
    >
      <span className="tile">
        {/* the face is masked around the status dot on translucent surfaces (styles/glass) */}
        <span className="gl-tile-face">{c.os ? <OsGlyph os={c.os} size={16} /> : <Icon name="monitor" size={16} />}</span>
        <i className={cx("pd", (off || c.online === null) && "off")} />
      </span>
      <span className="tx">
        <span className="nm">{c.host.name}</span>
        <span className={cx("st", st.tone)}>{st.text}</span>
      </span>
      {spec && (
        <span className="mini">
          <TurnLine spec={spec} w={width - 24 - 20 - 30 - 12} mini />
        </span>
      )}
    </button>
  );
}

const ORDER: Group[] = ["needs", "you", "working", "idle", "offline"];
const GROUP_LABEL: Record<Group, "g_needs" | "g_you" | "g_working" | "g_idle" | "g_offline"> = {
  needs: "g_needs",
  you: "g_you",
  working: "g_working",
  idle: "g_idle",
  offline: "g_offline",
};

const OFFLINE = "ai-remote.sidebar.offline";

/** Whether the offline group is open: what the person chose, else open below three computers. */
function useOfflineOpen(count: number): [boolean, () => void] {
  const [saved, setSaved] = useState(() => localStorage.getItem(OFFLINE));
  const open = saved === "open" ? true : saved === "closed" ? false : count < 3;
  return [
    open,
    () => {
      const next = open ? "closed" : "open";
      localStorage.setItem(OFFLINE, next);
      setSaved(next);
    },
  ];
}

/** The sidebar's own toggle: fixed at one spot so it never moves under the pointer (macOS: right of
 *  the traffic lights; Windows: in the strip). Never role=switch: that role belongs to settings. */
export function SidebarToggle({ state, onToggle }: { state: SidebarState; onToggle: () => void }) {
  const label = t(state === "expanded" ? "sidebar_collapse" : "sidebar_expand");
  return (
    <button
      type="button"
      className="gl-sidetoggle"
      onClick={onToggle}
      aria-label={label}
      aria-expanded={state === "expanded"}
      aria-controls="gl-side"
      title={`${label} · ${shortcutLabel("sidebar", os)}`}
    >
      <Icon name="sidebar" size={16} />
    </button>
  );
}

export function Sidebar({
  computers,
  active,
  waiting,
  now,
  query,
  onQuery,
  onNav,
  state,
  onToggle,
  onSearch,
  searchRef,
  onMode,
}: {
  computers: Computer[];
  active: NavKey | null;
  waiting: number;
  now: number;
  query: string;
  onQuery: (query: string) => void;
  onNav: (n: NavKey) => void;
  state: SidebarState;
  onToggle: () => void;
  /** The compact sidebar's search button: expand and focus the field. */
  onSearch: () => void;
  searchRef?: RefObject<HTMLInputElement | null>;
  /** The computers / agents switch at the top: "agents" opens the agents workspace. */
  onMode?: (mode: AppMode) => void;
}) {
  const width = 240;
  const own = useRef<HTMLInputElement | null>(null);
  const input = searchRef ?? own;
  const isActive = (n: NavKey) =>
    typeof n === "string" ? active === n : typeof active === "object" && active?.computer === n.computer;
  const q = query.trim().toLocaleLowerCase();
  const groups = ORDER.map((g) => [g, computers.filter((c) => groupOf(c, now) === g && c.host.name.toLocaleLowerCase().includes(q))] as const).filter(
    ([, list]) => list.length > 0,
  );
  const offlineCount = groups.find(([g]) => g === "offline")?.[1].length ?? 0;
  const [offlineOpen, toggleOffline] = useOfflineOpen(offlineCount);
  const hint = (key: "page1" | "page2" | "page3" | "settings" | "pair") => ` · ${shortcutLabel(key, os)}`;
  const navRow = (n: "devices" | "approvals" | "activity" | "pair" | "settings", icon: ReactNode, label: string, key: Parameters<typeof hint>[0], end?: ReactNode) => (
    <button
      type="button"
      className={cx("nv", isActive(n) && "sel")}
      onClick={() => onNav(n)}
      aria-current={isActive(n) ? "page" : undefined}
      title={label + hint(key)}
    >
      {icon}
      <span className="lbl">{label}</span>
      {end}
    </button>
  );
  return (
    <nav className="side" id="gl-side" aria-label={t("main_navigation")} data-state={state}>
      {/* The window's traffic lights (macOS) sit here; the strip drags the window. */}
      <div className="nt-drag gl-side-top" data-tauri-drag-region>
        {os === "windows" && (
          <span className="gl-appname" data-tauri-drag-region>
            <Mark h={14} />
            <span>{t("app")}</span>
          </span>
        )}
        <SidebarToggle state={state} onToggle={onToggle} />
      </div>
      {/* At the strip's right end while expanded; under it, stacked, while compact (styles/workspace.css). */}
      {onMode && (
        <div className="gl-side-modes">
          <ModeSwitch mode="computers" onMode={onMode} />
        </div>
      )}
      <div className="search">
        <Icon name="search" size={14} />
        <input
          ref={input}
          type="search"
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          placeholder={t("search_computers")}
          aria-label={t("search_computers")}
        />
        {query ? (
          <button type="button" className="search-clear" onClick={() => onQuery("")} aria-label={t("clear_search")}>
            <Icon name="x" size={12} />
          </button>
        ) : (
          <span className="k">{shortcutLabel("search", os)}</span>
        )}
      </div>
      <button type="button" className="gl-side-search" onClick={onSearch} aria-label={t("search_computers")} title={`${t("search_computers")} · ${shortcutLabel("search", os)}`}>
        <Icon name="search" size={16} />
      </button>
      {navRow("devices", <Icon name="monitor" size={16} />, t("devices"), "page1", computers.length > 0 && <span className="end device-count">{computers.length}</span>)}
      {navRow(
        "approvals",
        <Glyph name="approval" size={16} />,
        t("approvals"),
        "page2",
        waiting > 0 && (
          <span className="end badge" aria-label={t("sidebar_attention", { n: waiting })}>
            {waiting}
          </span>
        ),
      )}
      {navRow("activity", <Icon name="history" size={16} />, t("activity"), "page3")}
      <div className="side-devices">
        {groups.length === 0 ? (
          <>
            <div className="side-h">{t("computers")}</div>
            <div className="empty-note" role="status">
              {t(computers.length === 0 ? "no_computers_short" : "search_no_matches")}
            </div>
          </>
        ) : (
          groups.map(([g, list]) => {
            const folds = g === "offline" && !q;
            const open = !folds || offlineOpen;
            return (
              <div key={g} className={cx("side-g", `g-${g}`)}>
                {folds ? (
                  <button type="button" className="side-h gl-disclosure" aria-expanded={open} onClick={toggleOffline}>
                    <Icon name="chevR" size={10} className="gl-chev" />
                    <span className="lbl">{t(GROUP_LABEL[g])}</span>
                    <span className="n">{list.length}</span>
                  </button>
                ) : (
                  <div className="side-h">
                    <span className="lbl">{t(GROUP_LABEL[g])}</span>
                    {/* a count only says something above one row (the folding group keeps it: its rows may be hidden) */}
                    {list.length > 1 && <span className="n">{list.length}</span>}
                  </div>
                )}
                {open &&
                  list.map((c) => (
                    <PcRow
                      key={c.host.name}
                      c={c}
                      now={now}
                      width={width}
                      sel={isActive({ computer: c.host.name })}
                      onClick={() => onNav({ computer: c.host.name })}
                    />
                  ))}
              </div>
            );
          })
        )}
      </div>
      {navRow("pair", <Icon name="add" size={16} />, t("pair"), "pair")}
      {navRow("settings", <Icon name="settings" size={16} />, t("settings"), "settings")}
    </nav>
  );
}
