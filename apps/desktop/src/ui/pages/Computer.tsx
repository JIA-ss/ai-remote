// The computer page: its news in one large sentence, the live preview and recent steps (Overview),
// this device's access, the tools on offer and the coding agents.

import { useState } from "react";
import { currentLang, t, type Key } from "../../lib/i18n";
import { groupOf, type Computer } from "../../lib/store";
import type { Step } from "../../lib/types";
import type { ChatTarget } from "../AgentChat";
import { AccessTab, AgentsTab, ToolsTab } from "../ComputerTabs";
import { DecisionCard } from "../Decide";
import { OsGlyph, Pointer, SeatMark } from "../glyphs";
import { Icon } from "../Icon";
import { Stream } from "../Session";
import { computerFacts, Facts, statusOf } from "../Shell";
import { TurnLine, turnSpec } from "../TurnLine";
import { agentLabel, clock, cx, relayNote, useNow, useShortWindow, useSize } from "../util";
import { levelName } from "./common";

/** The computer page's headline and lede: what is happening there, in one sentence. */
function news(c: Computer, now: number): { hl: string; lede: string | null; who: "you" | "agent" | "idle" } {
  const host = c.host.name;
  const agent = agentLabel(c.feed.agent);
  const g = groupOf(c, now);
  const p = c.feed.pending[0];
  if (g === "offline") return { hl: t("hl_offline", { host }), lede: c.problem, who: "idle" };
  if (p) {
    const asked = c.feed.steps.find((s) => s.approvalId === p.id);
    const hl =
      p.tool === "shell"
        ? t("hl_waiting_shell")
        : p.tool === "jobs_start"
          ? t("hl_waiting_job")
          : p.tool.startsWith("files_")
            ? t("hl_waiting_file")
            : t("hl_waiting_tool", { tool: p.tool });
    return { hl, lede: t("lede_waiting", { agent, time: clock(asked?.startedMs ?? now), tool: p.tool }), who: "you" };
  }
  if (g === "you") return { hl: t("hl_you", { host }), lede: t("lede_you", { agent }), who: "you" };
  if (c.feed.driver.state === "paused")
    return { hl: t("hl_paused", { host }), lede: t(c.feed.driver.owner ? "lede_owner_paused" : "lede_paused", { agent, host }), who: "idle" };
  const last = c.feed.steps.at(-1);
  if (g === "working" && last)
    return {
      hl: t("hl_working", { agent, host }),
      lede: t("lede_working", { time: clock(last.startedMs), step: last.intent ?? last.tool }),
      who: "agent",
    };
  if (c.online === null) return { hl: t("hl_connecting", { host }), lede: null, who: "idle" };
  return {
    hl: t("hl_idle", { host }),
    lede: last ? t("lede_idle", { time: clock(last.startedMs, false) }) : t("lede_idle_none"),
    who: "idle",
  };
}

function StepRows({ steps }: { steps: Step[] }) {
  return (
    <div className="log">
      {steps.map((s) => (
        <div
          key={s.key}
          className={cx("lr", s.byPerson ? "you u-you d-you" : "done u-agent d-agent", s.state === "waiting" && "n-waithead")}
        >
          <span className="t">{clock(s.startedMs)}</span>
          <span className="r">{s.state === "waiting" && <i />}</span>
          <div className="x">
            {s.state === "waiting" ? <span className="strong">{t("s_asked", { tool: s.tool })}</span> : (s.intent ?? s.tool)}
          </div>
        </div>
      ))}
    </div>
  );
}

type Tab = "overview" | "access" | "tools" | "agents";

/** `?tab=` opens a tab directly (screenshots of the page outside the app). */
function initialTab(): Tab {
  const v = new URLSearchParams(location.search).get("tab");
  return v === "access" || v === "tools" || v === "agents" ? v : "overview";
}

/** One computer: what is happening there (Overview), this device's access, the tools on offer. */
export function ComputerPage({
  c,
  onWatch,
  onActivity,
  onUnpaired,
  onOpenChat,
}: {
  c: Computer;
  onWatch: () => void;
  onActivity: () => void;
  onUnpaired: () => void;
  /** Opens a coding-agent chat beside the live view. */
  onOpenChat: (target: ChatTarget) => void;
}) {
  const [tab, setTab] = useState<Tab>(initialTab);
  const off = c.online === false;
  const short = useShortWindow();
  const tabs: Array<[Tab | "activity", Key]> = [
    ["overview", "tab_overview"],
    ["access", "tab_access"],
    ["tools", "tab_tools"],
    ["agents", "tab_agents"],
    ["activity", "tab_activity"],
  ];
  return (
    <div className={cx("cpage", short && "short")}>
      <div className="cp-top">
        <span className="tile">
          <OsGlyph os={c.os ?? "windows"} size={16} />
          <i className={cx("pd", (off || c.online === null) && "off")} />
        </span>
        <span className="nm">{c.host.name}</span>
        <span className="facts">
          <Facts items={computerFacts(c)} />
          <span>
            <i className={c.online === true ? "on" : "off"} />
            {off ? t("st_offline") : c.online === null ? t("st_connecting") : t("live")}
          </span>
        </span>
        <div className="seg fit" role="tablist">
          {tabs.map(([k, label]) => (
            <button
              type="button"
              key={k}
              className={cx(k === tab && "sel")}
              role="tab"
              tabIndex={0}
              aria-selected={k === tab}
              onClick={() => (k === "activity" ? onActivity() : setTab(k))}
            >
              {t(label)}
            </button>
          ))}
        </div>
      </div>
      {tab === "access" ? (
        <AccessTab c={c} onUnpaired={onUnpaired} />
      ) : tab === "tools" ? (
        <ToolsTab c={c} />
      ) : tab === "agents" ? (
        <AgentsTab c={c} onOpenChat={onOpenChat} />
      ) : (
        <Overview c={c} short={short} onWatch={onWatch} onActivity={onActivity} />
      )}
    </div>
  );
}

function Overview({ c, short, onWatch, onActivity }: { c: Computer; short: boolean; onWatch: () => void; onActivity: () => void }) {
  const now = useNow(1000);
  const st = statusOf(c, now);
  const agent = agentLabel(c.feed.agent);
  const pending = c.feed.pending[0];
  const askedStep = pending ? c.feed.steps.find((s) => s.approvalId === pending.id) : undefined;
  const [pvRef, pv] = useSize<HTMLDivElement>();
  const spec = turnSpec(c.feed, now, c.online !== false);
  const off = c.online === false;
  const group = groupOf(c, now);
  const n = news(c, now);
  // Below 860 px tall the footer keeps two columns: the facts are in the top line and the Access tab.
  const recent = short ? (currentLang() === "zh" ? 2 : 3) : 4;
  return (
    <>
      <div className="cp-hl">
        {n.who === "you" ? <Pointer who="you" h={28} ui /> : n.who === "agent" ? <Pointer who="agent" h={28} ui /> : <SeatMark state="idle" h={24} />}
        <div className="dsp">{n.hl}</div>
      </div>
      {n.lede && <div className="cp-lede lede">{n.lede}</div>}
      <div className="cp-grid">
        {pending ? <DecisionCard p={pending} host={c.host.name} agent={agent} askedMs={askedStep?.startedMs} wide /> : <div />}
        <div className="pv-wrap">
          <div className="pv stage" ref={pvRef}>
            <div className={cx("shot", off && "desat")} style={{ height: Math.round((((pv.w || 400) - 24) * 10) / 16) }}>
              {!off && <Stream host={c.host.name} maxFps={2} maxLongEdge={960} />}
              <div className={cx("frame", c.feed.driver.state === "person" ? "you" : group === "working" || group === "needs" ? "agent" : "idle")} />
            </div>
            {spec && (
              <div className="pvline">
                <span className="tnum">{clock(Math.min(...c.feed.steps.map((s) => s.startedMs), now), false)}</span>
                <TurnLine spec={spec} w={Math.max((pv.w || 400) - 24 - 48, 0)} />
              </div>
            )}
            <div className="cap2">
              <SeatMark state={group === "needs" ? "waiting" : group === "you" ? "you" : group === "working" ? "agent" : "idle"} h={18} />
              <div style={{ display: "flex", flexDirection: "column", minWidth: 0 }}>
                <span className="l1">{st.text}</span>
                <span className="l2">{off ? t("st_offline") : `${t("live")} · ${clock(now, false)}`}</span>
              </div>
              {!off && (
                <button className="watch" onClick={onWatch}>
                  <Icon name="eye" size={14} />
                  {t("watch_live")}
                </button>
              )}
            </div>
          </div>
        </div>
      </div>
      <div className={cx("cp-cols", short && "two")}>
        <div>
          <div className="colh">{t("cur_task")}</div>
          {c.feed.task ? (
            <>
              <div className="body">{c.feed.task.text}</div>
              <div className="cap c3" style={{ marginTop: 6 }}>
                {t("cur_task_by", { agent, time: clock(c.feed.task.ts, false), device: c.feed.task.peer })}
              </div>
            </>
          ) : (
            <div className="body c3">{c.feed.agent ? agent : "—"}</div>
          )}
        </div>
        <div>
          <div className="colh">
            {t("recent_steps")}
            <button className="end" onClick={onActivity}>
              {t("see_all")}
            </button>
          </div>
          {c.feed.steps.length === 0 ? <div className="body c3">{t("no_steps")}</div> : <StepRows steps={[...c.feed.steps].reverse().slice(0, recent)} />}
        </div>
        {!short && (
          <div>
            <div className="colh">{t("this_computer")}</div>
            <dl className="dl">
              <dt>{t("connection")}</dt>
              <dd title={relayNote(c.host)?.detail}>
                {c.host.path ? t(c.host.path === "relay" ? (c.host.relayN0 ? "conn_relayed_n0" : "conn_relayed") : "conn_direct") : "—"}
              </dd>
              <dt>{t("your_level")}</dt>
              <dd>
                {levelName(c.feed.level !== undefined ? c.feed.level : c.host.levelAtPairing)}
                {c.feed.bypass && <span className="c-danger">{` · ${t("bypass_fact")}`}</span>}
              </dd>
              <dt>{t("device_key")}</dt>
              <dd className="mono">{c.host.device ? c.host.device.slice(0, 16) : "—"}</dd>
            </dl>
          </div>
        )}
      </div>
    </>
  );
}
