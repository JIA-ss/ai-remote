// The computer page: its news in one large sentence, the decision when one waits, the live preview
// (the picture opens the live view) and the current task with recent steps (Overview); this device's
// access with the computer's facts, the tools on offer and the coding agents (the tabs sit in the toolbar).

import { currentLang, t, type Key } from "../../lib/i18n";
import type { ComputerTab } from "../../lib/nav";
import { groupOf, type Computer } from "../../lib/store";
import type { Step } from "../../lib/types";
import type { ChatTarget } from "../AgentChat";
import { AccessTab, AgentsTab, ToolsTab } from "../ComputerTabs";
import { DecisionCard } from "../Decide";
import { SeatMark } from "../glyphs";
import { Icon } from "../Icon";
import { Stream } from "../Session";
import { statusOf } from "../Shell";
import { TurnLine, turnSpec } from "../TurnLine";
import { canWatch } from "../Devices";
import { agentLabel, clock, cx, useNow, useShortWindow, useSize } from "../util";

/** The computer page's headline and lede: what is happening there, in one sentence. `detail` is the
 *  host's own wording of a problem (English, technical), kept for the lede's tooltip. */
function news(c: Computer, now: number): { hl: string; lede: string | null; who: "you" | "agent" | "idle"; detail?: string } {
  const host = c.host.name;
  const agent = agentLabel(c.feed.agent);
  const g = groupOf(c, now);
  const p = c.feed.pending[0];
  if (g === "offline") return { hl: t("hl_offline", { host }), lede: t("cp_offline_d"), who: "idle", detail: c.problem ?? undefined };
  if (p) {
    const hl =
      p.tool === "shell"
        ? t("hl_waiting_shell")
        : p.tool === "jobs_start"
          ? t("hl_waiting_job")
          : p.tool.startsWith("files_")
            ? t("hl_waiting_file")
            : t("hl_waiting_tool", { tool: p.tool });
    // the decision card right below says who asks, what, and when: no second sentence here
    return { hl, lede: null, who: "you" };
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
    lede: last ? t("lede_idle", { time: clock(last.startedMs, false) }) : t("cp_idle_none"),
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

const TABS: Array<[ComputerTab, Key]> = [
  ["overview", "tab_overview"],
  ["access", "tab_access"],
  ["tools", "tab_tools"],
  ["agents", "tab_agents"],
];

/** The computer page's tabs, drawn in the shell's toolbar (a tab change replaces the route, it adds
 *  no history). Arrow keys move between them, Home / End jump to the ends. */
export function ComputerTabStrip({ tab, onTab }: { tab: ComputerTab; onTab: (tab: ComputerTab) => void }) {
  const move = (e: React.KeyboardEvent, i: number) => {
    const n = TABS.length;
    const to = e.key === "ArrowRight" ? (i + 1) % n : e.key === "ArrowLeft" ? (i - 1 + n) % n : e.key === "Home" ? 0 : e.key === "End" ? n - 1 : -1;
    if (to < 0) return;
    e.preventDefault();
    onTab(TABS[to][0]);
    e.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role=tab]")[to]?.focus();
  };
  return (
    <div className="gl-cap gl-glass thin gl-segcap">
      <div className="gl-seg" role="tablist">
        {TABS.map(([k, label], i) => (
          <button
            type="button"
            key={k}
            className={cx("gl-segbtn", k === tab && "sel")}
            role="tab"
            tabIndex={k === tab ? 0 : -1}
            aria-selected={k === tab}
            onClick={() => onTab(k)}
            onKeyDown={(e) => move(e, i)}
          >
            <span className="lbl">{t(label)}</span>
          </button>
        ))}
      </div>
    </div>
  );
}

/** One computer: what is happening there (Overview), this device's access, the tools on offer, the
 *  coding agents. Its name, facts and tabs are in the shell's toolbar. */
export function ComputerPage({
  c,
  tab,
  onWatch,
  onActivity,
  onUnpaired,
  onOpenChat,
}: {
  c: Computer;
  tab: ComputerTab;
  onWatch: () => void;
  /** This computer's steps on the activity page. */
  onActivity: (host: string) => void;
  onUnpaired: () => void;
  /** Opens a coding-agent chat beside the live view. */
  onOpenChat: (target: ChatTarget) => void;
}) {
  const short = useShortWindow();
  return (
    <div className={cx("cpage", short && "short")}>
      {tab === "access" ? (
        <AccessTab c={c} onUnpaired={onUnpaired} />
      ) : tab === "tools" ? (
        <ToolsTab c={c} />
      ) : tab === "agents" ? (
        <AgentsTab c={c} onOpenChat={onOpenChat} />
      ) : (
        <Overview c={c} short={short} onWatch={onWatch} onActivity={() => onActivity(c.host.name)} />
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
  const watch = canWatch(c);
  const group = groupOf(c, now);
  const n = news(c, now);
  const recent = short ? (currentLang() === "zh" ? 3 : 4) : 5;
  // The task and the recent steps: under the decision when one waits, beside the picture otherwise.
  const side = (
    <div className={cx("cp-cols", pending ? "two" : "gl-aside")}>
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
        <div className="colh">{t("recent_steps")}</div>
        {c.feed.steps.length === 0 ? <div className="body c3">{t("no_steps")}</div> : <StepRows steps={[...c.feed.steps].reverse().slice(0, recent)} />}
        <button type="button" className="gl-more" onClick={onActivity}>
          {t("activity_of_host")}
          <Icon name="chevR" size={12} />
        </button>
      </div>
    </div>
  );
  return (
    <>
      {/* the same heading as the other tabs and pairing (glass/pages.css): no mark in front of it */}
      <div className="cp-hl">
        <div className="dsp">{n.hl}</div>
      </div>
      {n.lede && <div className="cp-lede lede" title={n.detail}>{n.lede}</div>}
      <div className={cx("cp-grid", !pending && "calm")}>
        {pending && <DecisionCard p={pending} host={c.host.name} agent={agent} askedMs={askedStep?.startedMs} wide />}
        <div className="pv-wrap">
          <div className="pv stage" ref={pvRef}>
            <div className={cx("shot", off && "desat")} style={{ height: Math.round((((pv.w || 400) - 24) * 10) / 16) }}>
              {!off && <Stream host={c.host.name} maxFps={2} maxLongEdge={960} />}
              <div className={cx("frame", c.feed.driver.state === "person" ? "you" : group === "working" || group === "needs" ? "agent" : "idle")} />
              {/* The picture is the card's one action: it opens the live view, as the toolbar's button does. */}
              {watch && (
                <button type="button" className="gl-pv-open" onClick={onWatch} aria-label={t("devices_watch_host", { host: c.host.name })}>
                  <span className="gl-pv-go">
                    <Icon name="eye" size={14} />
                    {t("watch_live")}
                  </span>
                </button>
              )}
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
                {/* while a request waits, the card above already says so: the picture names the agent */}
                <span className="l1">{pending ? agent : st.text}</span>
                <span className="l2">{off ? t("st_offline") : `${t("live")} · ${clock(now, false)}`}</span>
              </div>
            </div>
          </div>
        </div>
        {!pending && side}
      </div>
      {pending && side}
    </>
  );
}
