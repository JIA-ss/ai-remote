// Window chrome and navigation: the 48 px title strip at the top of the content area (caption
// buttons come from the window frame on Windows; on macOS the traffic lights sit over the sidebar's
// own strip), and the sidebar with the computer list, flush with the window's left edge.

import type { ReactNode } from "react";
import { api } from "../lib/api";
import { t } from "../lib/i18n";
import { groupOf, type Computer, type Group } from "../lib/store";
import { Glyph, Mark, OsGlyph } from "./glyphs";
import { Icon } from "./Icon";
import { TurnLine, turnSpec } from "./TurnLine";
import { clock, cx, routeLabel } from "./util";

export type Nav = "devices" | "approvals" | "activity" | "pair" | "settings" | { computer: string };

export function TitleBar({ title, facts, onBack, end }: { title?: string; facts?: ReactNode; onBack?: () => void; end?: ReactNode }) {
  return (
    <header className="tb" data-tauri-drag-region>
      {onBack && (
        <button className="back" onClick={onBack} aria-label={t("back")}>
          <Icon name="back" size={14} />
        </button>
      )}
      <div className="app" data-tauri-drag-region>
        <Mark h={16} />
        {title ? (
          <>
            <span className="title">{title}</span>
            {facts && (
              <span className="facts" style={{ marginLeft: 6 }}>
                {facts}
              </span>
            )}
          </>
        ) : (
          <span className="name">{t("app")}</span>
        )}
      </div>
      {api.kind === "demo" && <span className="demo-badge">{t("preview_badge")}</span>}
      <div className="drag" data-tauri-drag-region />
      {end && <div className="nt-tb-end">{end}</div>}
      {/* Room for the caption buttons drawn by the window frame. */}
      <div style={{ width: "var(--tauri-frame-controls-width, 0px)", flex: "none" }} />
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
    <button type="button" className={cx("pc", sel && "sel", off && "off")} title={c.host.name} onClick={onClick} aria-current={sel ? "page" : undefined}>
      <span className="tile">
        {c.os ? <OsGlyph os={c.os} size={16} /> : <Icon name="monitor" size={16} />}
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

const ORDER: Group[] = ["needs", "working", "you", "idle", "offline"];
const GROUP_LABEL: Record<Group, Parameters<typeof t>[0]> = {
  needs: "g_needs",
  working: "g_working",
  you: "g_working",
  idle: "g_idle",
  offline: "g_offline",
};

export function Sidebar({
  computers,
  active,
  waiting,
  now,
  query,
  onQuery,
  onNav,
}: {
  computers: Computer[];
  active: Nav | null;
  waiting: number;
  now: number;
  query: string;
  onQuery: (query: string) => void;
  onNav: (n: Nav) => void;
}) {
  const width = 240;
  const isActive = (n: Nav) =>
    typeof n === "string" ? active === n : typeof active === "object" && active?.computer === n.computer;
  // "you" rows sit with the working ones under one heading.
  const grouped = new Map<string, Computer[]>();
  for (const g of ORDER) {
    const label = GROUP_LABEL[g];
    const list = computers.filter((c) => groupOf(c, now) === g && c.host.name.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()));
    if (list.length) grouped.set(label, [...(grouped.get(label) ?? []), ...list]);
  }
  return (
    <nav className="side" aria-label={t("main_navigation")}>
      {/* The window's traffic lights (macOS) sit here; the strip drags the window. */}
      <div className="nt-drag" data-tauri-drag-region />
      <div className="search">
        <Icon name="search" size={14} />
        <input type="search" value={query} onChange={(e) => onQuery(e.target.value)} placeholder={t("search_computers")} aria-label={t("search_computers")} />
        {query && <button type="button" className="search-clear" onClick={() => onQuery("")} aria-label={t("clear_search")}><Icon name="x" size={12} /></button>}
      </div>
      <button type="button" className={cx("nv", isActive("devices") && "sel")} onClick={() => onNav("devices")} aria-current={isActive("devices") ? "page" : undefined}>
        <Icon name="monitor" size={16} />
        <span>{t("devices")}</span>
        <span className="end device-count">{computers.length}</span>
      </button>
      <button type="button" className={cx("nv", isActive("approvals") && "sel")} onClick={() => onNav("approvals")} aria-current={isActive("approvals") ? "page" : undefined}>
        <Glyph name="approval" size={16} />
        <span>{t("approvals")}</span>
        {waiting > 0 && <span className="end badge">{waiting}</span>}
      </button>
      <button type="button" className={cx("nv", isActive("activity") && "sel")} onClick={() => onNav("activity")} aria-current={isActive("activity") ? "page" : undefined}>
        <Icon name="history" size={16} />
        <span>{t("activity")}</span>
      </button>
      <div className="side-devices">
      {computers.length === 0 || grouped.size === 0 ? (
        <>
          <div className="side-h">{t("computers")}</div>
          <div className="empty-note" role="status">{t(computers.length === 0 ? "no_computers_short" : "search_no_matches")}</div>
        </>
      ) : (
        [...grouped.entries()].map(([label, list]) => (
          <div key={label}>
            <div className="side-h">
              {t(label as Parameters<typeof t>[0])}
              <span className="n">{list.length}</span>
            </div>
            {list.map((c) => (
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
        ))
      )}
      </div>
      <button type="button" className={cx("nv", isActive("pair") && "sel")} onClick={() => onNav("pair")} aria-current={isActive("pair") ? "page" : undefined}>
        <Icon name="add" size={16} />
        <span>{t("pair")}</span>
      </button>
      <button type="button" className={cx("nv", isActive("settings") && "sel")} onClick={() => onNav("settings")} aria-current={isActive("settings") ? "page" : undefined}>
        <Icon name="settings" size={16} />
        <span>{t("settings")}</span>
      </button>
    </nav>
  );
}
