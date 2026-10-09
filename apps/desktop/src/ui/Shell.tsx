// Window chrome and navigation: the 48 px title bar (caption buttons come from the window frame),
// the NavigationView pane with the computer list, and the compact rail used in sessions.

import type { ReactNode } from "react";
import { t } from "../lib/i18n";
import { groupOf, type Computer, type Group } from "../lib/store";
import { Glyph, Mark, OsGlyph } from "./glyphs";
import { Icon } from "./Icon";
import { TurnLine, turnSpec } from "./TurnLine";
import { clock, cx } from "./util";

export type Nav = "approvals" | "activity" | "pair" | "settings" | { computer: string };

export function TitleBar({ title, facts, onBack }: { title?: string; facts?: ReactNode; onBack?: () => void }) {
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
      <div className="drag" data-tauri-drag-region />
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
  const lvl = c.host.levelAtPairing;
  if (lvl) out.push([<Glyph key="lvl" name={levelGlyph[lvl]} size={12} />, t(levelName[lvl])]);
  if (c.host.path === "direct" || c.host.path === "relay")
    out.push([<Glyph key="p" name={c.host.path === "relay" ? "relayed" : "direct"} size={12} />, t(c.host.path === "relay" ? "relayed" : "direct")]);
  return out;
}

/** The status sentence of a computer row and its tone. */
export function statusOf(c: Computer, now: number): { text: string; tone: "" | "you" | "agent" } {
  const g = groupOf(c, now);
  if (g === "offline") return { text: c.since ? t("st_offline_since", { time: clock(c.since, false) }) : t("st_offline"), tone: "" };
  if (g === "needs") return { text: t("st_waiting"), tone: "you" };
  if (g === "you") return { text: t("st_you"), tone: "you" };
  if (c.feed.driver.state === "paused") return { text: t("st_paused"), tone: "" };
  if (g === "working") {
    const last = c.feed.steps.at(-1);
    return { text: last?.intent ?? t("st_running", { tool: last?.tool ?? "" }), tone: "agent" };
  }
  if (c.online === null) return { text: t("st_connecting"), tone: "" };
  return { text: c.since ? t("st_idle_since", { time: clock(c.since, false) }) : t("st_idle"), tone: "" };
}

function PcRow({ c, sel, now, onClick, width }: { c: Computer; sel: boolean; now: number; onClick: () => void; width: number }) {
  const st = statusOf(c, now);
  const spec = Object.keys(c.feed.sessions).length || c.feed.pending.length ? turnSpec(c.feed, now, c.online !== false) : null;
  const off = c.online === false;
  return (
    <div className={cx("pc", sel && "sel", off && "off")} title={c.host.name} onClick={onClick} role="button" tabIndex={0}>
      <span className="tile">
        <OsGlyph os={c.os ?? "windows"} size={16} />
        <i className={cx("pd", (off || c.online === null) && "off")} />
      </span>
      <div className="tx">
        <span className="nm">{c.host.name}</span>
        <span className={cx("st", st.tone)}>{st.text}</span>
      </div>
      {spec && (
        <div className="mini">
          <TurnLine spec={spec} w={width - 24 - 20 - 30 - 12} mini />
        </div>
      )}
    </div>
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
  onNav,
}: {
  computers: Computer[];
  active: Nav | null;
  waiting: number;
  now: number;
  onNav: (n: Nav) => void;
}) {
  const width = 272;
  const isActive = (n: Nav) =>
    typeof n === "string" ? active === n : typeof active === "object" && active?.computer === n.computer;
  // "you" rows sit with the working ones under one heading.
  const grouped = new Map<string, Computer[]>();
  for (const g of ORDER) {
    const label = GROUP_LABEL[g];
    const list = computers.filter((c) => groupOf(c, now) === g);
    if (list.length) grouped.set(label, [...(grouped.get(label) ?? []), ...list]);
  }
  return (
    <nav className="side" style={{ width }}>
      <div className="search">
        <Icon name="search" size={14} />
        <span>{t("search_computers")}</span>
      </div>
      <div className={cx("nv", isActive("approvals") && "sel")} onClick={() => onNav("approvals")} role="button" tabIndex={0}>
        <Glyph name="approval" size={16} />
        <span>{t("approvals")}</span>
        {waiting > 0 && <span className="end badge">{waiting}</span>}
      </div>
      <div className={cx("nv", isActive("activity") && "sel")} onClick={() => onNav("activity")} role="button" tabIndex={0}>
        <Icon name="history" size={16} />
        <span>{t("activity")}</span>
      </div>
      {computers.length === 0 ? (
        <>
          <div className="side-h">{t("computers")}</div>
          <div className="empty-note">{t("no_computers_short")}</div>
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
      <div className="spacer" />
      <div className={cx("nv", isActive("pair") && "sel")} onClick={() => onNav("pair")} role="button" tabIndex={0}>
        <Icon name="add" size={16} />
        <span>{t("pair")}</span>
      </div>
      <div className={cx("nv", isActive("settings") && "sel")} onClick={() => onNav("settings")} role="button" tabIndex={0}>
        <Icon name="settings" size={16} />
        <span>{t("settings")}</span>
      </div>
    </nav>
  );
}

/** The compact rail in sessions. */
export function NavRail({ waiting, os, onNav }: { waiting: number; os: "windows" | "mac"; onNav: (n: Nav | "home") => void }) {
  return (
    <nav className="nrail">
      <div className="ni" onClick={() => onNav("home")} role="button" tabIndex={0}>
        <Icon name="menu" size={16} />
      </div>
      <div className="ni" onClick={() => onNav("approvals")} role="button" tabIndex={0}>
        <Glyph name="approval" size={18} />
        {waiting > 0 && <span className="badge">{waiting}</span>}
      </div>
      <div className="ni" onClick={() => onNav("activity")} role="button" tabIndex={0}>
        <Icon name="history" size={16} />
      </div>
      <div className="sep" />
      <div className="ni sel">
        <OsGlyph os={os} size={16} />
      </div>
      <div className="spacer" />
      <div className="ni" onClick={() => onNav("pair")} role="button" tabIndex={0}>
        <Icon name="add" size={16} />
      </div>
      <div className="ni" onClick={() => onNav("settings")} role="button" tabIndex={0}>
        <Icon name="settings" size={16} />
      </div>
    </nav>
  );
}
