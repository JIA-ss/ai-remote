// Main-window pages, after the final design: each states its news in one large sentence, the rest
// sits on one text edge. The computer page, first run, pairing, the approvals inbox, activity and
// settings.

import { useMemo, useState } from "react";
import { api, type Paired } from "../lib/api";
import { currentLang, t, type Key } from "../lib/i18n";
import { allPending, groupOf, type Computer, type Snapshot } from "../lib/store";
import type { Step } from "../lib/types";
import { CmdLine } from "./CmdLine";
import { AccessTab, ToolsTab } from "./ComputerTabs";
import { DecisionCard } from "./Decide";
import { Glyph, OsGlyph, Pointer, SeatMark, Wordmark } from "./glyphs";
import { Icon } from "./Icon";
import { Stream } from "./Session";
import { computerFacts, Facts, statusOf } from "./Shell";
import { TurnLine, turnSpec, type TurnSpec } from "./TurnLine";
import { WelcomeFigure } from "./WelcomeFigure";
import { agentLabel, clock, cx, useNow, useShortWindow, useSize } from "./util";

const levelName = (l: string | null) =>
  l === "admin" ? t("lvl_admin") : l === "control" ? t("lvl_control") : l === "observe" ? t("lvl_observe") : "—";

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
      p.tool === "shell" ? t("hl_waiting_shell") : p.tool === "jobs_start" ? t("hl_waiting_job") : t("hl_waiting_tool", { tool: p.tool });
    return { hl, lede: t("lede_waiting", { agent, time: clock(asked?.startedMs ?? now), tool: p.tool }), who: "you" };
  }
  if (g === "you") return { hl: t("hl_you", { host }), lede: t("lede_you", { agent }), who: "you" };
  if (c.feed.driver.state === "paused") return { hl: t("hl_paused", { host }), lede: t("lede_paused", { agent }), who: "idle" };
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

type Tab = "overview" | "access" | "tools";

/** `?tab=` opens a tab directly (screenshots of the page outside the app). */
function initialTab(): Tab {
  const v = new URLSearchParams(location.search).get("tab");
  return v === "access" || v === "tools" ? v : "overview";
}

/** One computer: what is happening there (Overview), this device's access, the tools on offer. */
export function ComputerPage({
  c,
  onWatch,
  onActivity,
  onUnpaired,
}: {
  c: Computer;
  onWatch: () => void;
  onActivity: () => void;
  onUnpaired: () => void;
}) {
  const [tab, setTab] = useState<Tab>(initialTab);
  const off = c.online === false;
  const short = useShortWindow();
  const tabs: Array<[Tab | "activity", Key]> = [
    ["overview", "tab_overview"],
    ["access", "tab_access"],
    ["tools", "tab_tools"],
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
              <dd>{c.host.path ? t(c.host.path === "relay" ? "conn_relayed" : "conn_direct") : "—"}</dd>
              <dt>{t("your_level")}</dt>
              <dd>{levelName(c.feed.level !== undefined ? c.feed.level : c.host.levelAtPairing)}</dd>
              <dt>{t("device_key")}</dt>
              <dd className="mono">{c.host.device ? c.host.device.slice(0, 16) : "—"}</dd>
            </dl>
          </div>
        )}
      </div>
    </>
  );
}

/** Display text with phrase marks: `|` allows a break there (Chinese is set keep-all), a newline forces one. */
function phrases(s: string): React.ReactNode {
  return s.split("\n").map((line, i) => (
    <span key={i}>
      {i > 0 && <br />}
      {line.split("|").map((p, j) => (
        <span key={j}>
          {j > 0 && <wbr />}
          {p}
        </span>
      ))}
    </span>
  ));
}

/**
 * The computer list when it is empty (after the first run): the one command, the one action, and the
 * stage where the first computer will appear, framed in the idle colour with a key to the frame colours.
 */
export function EmptyPage({ onPair }: { onPair: () => void }) {
  const key = (c: string, k: "em_k_agent" | "em_k_you" | "em_k_idle") => (
    <span className="em-k">
      <i style={{ boxShadow: `inset 0 0 0 2px ${c}` }} />
      {t(k)}
    </span>
  );
  return (
    <div className="ipage em-page">
      <div className="cp-hl" style={{ marginTop: 0 }}>
        <div className="dsp">{t("em_title")}</div>
      </div>
      <div className="lede">{t("em_lede")}</div>
      <div className="em-grid">
        <div className="em-steps">
          <div className="em-st">
            <span className="n tnum">1</span>
            <div className="b">
              <div className="t">{t("em_s1")}</div>
              <CmdLine cmd="ai-remote host" />
              <div className="d">{t("em_s1_d")}</div>
            </div>
          </div>
          <div className="em-st">
            <span className="n tnum">2</span>
            <div className="b">
              <div className="t">{t("em_s2")}</div>
              <div>
                <button className="btn primary lg" onClick={onPair}>
                  <Icon name="add" size={14} />
                  {t("pair")}
                </button>
              </div>
              <div className="d">{t("em_s2_d")}</div>
            </div>
          </div>
        </div>
        <div className="em-stage stagebox stage">
          <div className="em-well">
            <div className="em-in">
              <SeatMark state="idle" h={44} />
              <div className="t">{t("em_stage")}</div>
              <div className="d">{t("em_stage_d")}</div>
              <div className="em-keys">
                {key("#6F9BFF", "em_k_agent")}
                {key("#F3B53F", "em_k_you")}
                {key("#8A919C", "em_k_idle")}
              </div>
            </div>
          </div>
          <div className="em-line">
            <span className="hl" />
            <SeatMark state="idle" h={16} />
          </div>
        </div>
      </div>
      <div style={{ flex: 1 }} />
      <div className="footnote">
        <Icon name="qr" size={12} />
        <span>{t("em_foot")}</span>
      </div>
    </div>
  );
}

/** First run: the product in one sentence, the two ways in, and the turn line told as a story. */
export function WelcomePage({ onPair }: { onPair: () => void }) {
  const [hostHelp, setHostHelp] = useState(false);
  const W = 616;
  const spec: TurnSpec = {
    // A finished story: the wait was answered, so it is the thin stroke with its decision dot, not dashes.
    segs: [
      ["agent", 0, 26],
      ["waited", 26, 46],
      ["agent", 46, 68],
      ["you", 68, 88],
      ["agent", 88, 100],
    ],
    ticks: [
      ["", 6],
      ["", 13],
      ["", 20],
      ["dec", 46],
      ["", 53],
      ["", 61],
      ["", 95],
    ],
    head: "agent",
  };
  const L = W - 18 * 0.62;
  const X = (p: number) => (p / 100) * L;
  const stops: Array<[number, "agent" | "you", string, "up" | "dn"]> = [
    [0, "agent", "fr_stop1", "up"],
    [26, "you", "fr_stop2", "dn"],
    [46, "you", "fr_stop3", "up"],
    [68, "you", "fr_stop4", "dn"],
    [88, "agent", "fr_stop5", "up"],
  ];
  return (
    <div className="spread" style={{ inset: 0, overflow: "auto" }}>
      <div className="lcol">
        <div>
          <Wordmark h={24} />
        </div>
        <div className="hero">{phrases(t("fr_title"))}</div>
        <div className="lede">{t("fr_sub")}</div>
        <div className="doors">
          <div className="door">
            <div className="ic">
              <SeatMark state="you" h={22} />
            </div>
            <div>
              <div className="ttl">{t("fr_ctl")}</div>
              <div className="d">{t("fr_ctl_d")}</div>
              <div className="a">
                <button className="btn primary lg" onClick={onPair}>
                  <Icon name="add" size={14} />
                  {t("pair")}
                </button>
              </div>
              <div className="note">{t("fr_ctl_note")}</div>
            </div>
          </div>
          <div className="door">
            <div className="ic">
              <SeatMark state="agent" h={22} />
            </div>
            <div>
              <div className="ttl">{t("fr_host")}</div>
              <div className="d">{t("fr_host_d")}</div>
              <div className="a">
                <button className="btn lg" onClick={() => setHostHelp(true)}>
                  {t("setup_this")}
                </button>
              </div>
              {hostHelp && (
                <>
                  <div className="note">{t("fr_host_cmd")}</div>
                  <CmdLine cmd="ai-remote host install" style={{ marginTop: 8 }} />
                </>
              )}
            </div>
          </div>
        </div>
      </div>
      <div>
        <div className="fig">
          <div className="stage-wrap">
            <div className="stagebox stage" style={{ height: 452 }}>
              <div className="capsule compact" style={{ top: 20, left: "50%", transform: "translateX(-50%)" }}>
                <span className="disc">
                  <SeatMark state="agent" h={20} />
                </span>
                <div className="who">
                  <span className="l1">{t("cap_agent_1", { agent: "Claude Code" })}</span>
                  <span className="l2">{t("cap_agent_2", { device: "home-laptop" })}</span>
                </div>
                <span className="pill take">{t("take_over")}</span>
                <span className="pill">
                  <Glyph name="paused" size={16} />
                  {t("pause_agent")}
                </span>
              </div>
              <div className="clip">
                <WelcomeFigure />
              </div>
              <div className="clipframe" />
            </div>
          </div>
          <div className="stops">
            {stops.map(([p, who, key, pos], i) => {
              const last = i === stops.length - 1;
              return (
                <div key={key} className={cx("lbl", pos)} style={last ? { right: 0, textAlign: "right" } : { left: Math.max(0, X(p) - 1) }}>
                  <b style={last ? { justifyContent: "flex-end" } : undefined}>
                    <Pointer who={who} h={11} ui />
                    {t(key as Parameters<typeof t>[0])}
                  </b>
                  {t(`${key}_d` as Parameters<typeof t>[0])}
                </div>
              );
            })}
            <div style={{ position: "absolute", left: 0, top: 63 }}>
              <TurnLine spec={spec} w={W} />
            </div>
          </div>
          <div className="figcap">{t("fr_fig")}</div>
          <div className="agentfoot" style={{ marginTop: 30 }}>
            <div style={{ paddingTop: 2, color: "var(--text-2)" }}>
              <Icon name="terminal" size={20} />
            </div>
            <div>
              <div className="strong">{t("fr_agent")}</div>
              <div className="body c2" style={{ marginTop: 2 }}>
                {t("fr_agent_d")}
              </div>
              <CmdLine cmd="claude mcp add ai-remote -- ai-remote mcp" />
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

/** Pairing: the ticket, then the safety words both screens show. */
export function PairPage({ onPaired }: { onPaired: (host: string) => void }) {
  const [ticket, setTicket] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [paired, setPaired] = useState<Paired | null>(null);
  const [step, setStep] = useState<1 | 2 | 3 | "mismatch">(1);
  const host = paired?.peer ?? (name.trim() || "—");

  const pair = async () => {
    setBusy(true);
    setError(null);
    try {
      const p = await api.pair(ticket.trim(), name.trim() || undefined);
      setPaired(p);
      setStep(2);
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  };

  const steps: Array<[number, string]> = [
    [1, t("pr_s1")],
    [2, t("pr_s2")],
    [3, t("pr_s3")],
  ];
  const cur = step === "mismatch" ? 2 : step;
  return (
    <div className="ppage" style={{ overflowY: "auto" }}>
      <div className="row" style={{ justifyContent: "space-between", maxWidth: 620 }}>
        <div className="strong">{t("pair")}</div>
        <div className="cap c3">{t("step_of", { n: cur, m: 3 })}</div>
      </div>
      <div className="steps4" style={{ marginTop: 14 }}>
        {steps.map(([n, label], i) => (
          <span key={n} style={{ display: "contents" }}>
            {i > 0 && <span className="bar" />}
            <span className={cx("s", n < cur && "done", n === cur && "cur")}>
              <span className="n">{n < cur ? <Icon name="check" size={10} /> : n}</span>
              {label}
            </span>
          </span>
        ))}
      </div>

      {step === 1 && (
        <div style={{ marginTop: 40, maxWidth: 560 }}>
          <div className="hd">{t("pr_door1")}</div>
          <div className="body c2" style={{ marginTop: 8 }}>
            {t("pr_door1_d")}
          </div>
          <textarea
            className="ta-input mono"
            rows={4}
            style={{ marginTop: 20 }}
            value={ticket}
            onChange={(e) => setTicket(e.target.value)}
            placeholder={t("pr_ticket_ph")}
          />
          <div className="body" style={{ marginTop: 18 }}>
            {t("pr_name")}
          </div>
          <input className="field lg ph-field" value={name} onChange={(e) => setName(e.target.value)} placeholder={t("pr_name_ph")} style={{ marginTop: 6, width: "100%" }} />
          {error && (
            <div className="conseq" style={{ marginTop: 14 }}>
              <Icon name="warn" size={16} />
              <span>{error}</span>
            </div>
          )}
          <div style={{ marginTop: 20 }}>
            <button className="btn primary lg" disabled={busy || !ticket.trim()} onClick={pair}>
              {busy ? t("pairing") : t("pair_btn")}
            </button>
          </div>
        </div>
      )}

      {step === 2 && paired && (
        <div style={{ marginTop: 56, maxWidth: 620 }}>
          <div className="hd">{t("pr_words_q", { host })}</div>
          <div className="body c2" style={{ marginTop: 8 }}>
            {t("pr_words_d")}
          </div>
          <div className="words" style={{ marginTop: 40 }}>
            {paired.sas.split(/[-\s]+/).join(" ")}
          </div>
          <div className="row" style={{ gap: 8, marginTop: 40 }}>
            <button className="btn primary lg" onClick={() => setStep(3)}>
              {t("match")}
            </button>
            <button
              className="btn lg"
              onClick={async () => {
                await api.unpair(paired.peer).catch(() => {});
                setStep("mismatch");
              }}
            >
              {t("no_match")}
            </button>
          </div>
          <div className="cap c2" style={{ marginTop: 28 }}>
            {t("pr_mismatch_note", { host })}
          </div>
        </div>
      )}

      {step === "mismatch" && (
        <div style={{ marginTop: 56, maxWidth: 620 }}>
          <div className="hd">{t("pr_err_words")}</div>
          <div className="body c2" style={{ marginTop: 8 }}>
            {t("pr_err_words_d", { host })}
          </div>
          <button
            className="btn lg"
            style={{ marginTop: 24 }}
            onClick={() => {
              setPaired(null);
              setTicket("");
              setStep(1);
            }}
          >
            {t("start_over")}
          </button>
        </div>
      )}

      {step === 3 && paired && (
        <div style={{ marginTop: 56, maxWidth: 620 }}>
          <div className="row" style={{ gap: 14 }}>
            <SeatMark state="both" h={32} />
            <div className="hd">{t("pr_done", { host })}</div>
          </div>
          <div className="body c2" style={{ marginTop: 10 }}>
            {t("pr_done_d", { level: levelName(paired.level) })}
          </div>
          <CmdLine cmd="claude mcp add ai-remote -- ai-remote mcp" style={{ marginTop: 20, maxWidth: 520 }} />
          <button className="btn primary lg" style={{ marginTop: 24 }} onClick={() => onPaired(paired.peer)}>
            {t("open_computer", { host })}
          </button>
        </div>
      )}
    </div>
  );
}

/** What a resolved approval's receipt says. */
function receiptOf(s: Step, host: string): { q: string; a: string; you: boolean } {
  const q = t("s_asked", { tool: s.tool });
  if (s.decision === "timeout" || s.decision === "unavailable") return { q, a: t("s_timeout"), you: false };
  const allowed = s.decision === "approved";
  if (s.answeredBy === "host") return { q, a: t(allowed ? "s_allowed_host" : "s_denied_host", { host }), you: false };
  return { q, a: t(allowed ? "s_allowed" : "s_denied", { device: s.answeredBy ?? "" }), you: true };
}

export function ApprovalsPage({ snap }: { snap: Snapshot }) {
  const list = allPending(snap);
  const today = new Date().toDateString();
  const decided = snap.computers
    .flatMap((c) => c.feed.steps.filter((s) => s.approvalId && s.state !== "waiting").map((s) => ({ s, host: c.host.name })))
    .filter(({ s }) => new Date(s.decidedMs ?? s.startedMs).toDateString() === today)
    .sort((a, b) => (b.s.decidedMs ?? 0) - (a.s.decidedMs ?? 0));
  return (
    <div className="ipage" style={{ overflowY: "auto" }}>
      <div className="cp-hl" style={{ marginTop: 0 }}>
        {list.length > 0 ? <Pointer who="you" h={28} ui /> : <SeatMark state="idle" h={24} />}
        <div className="dsp">{list.length === 0 ? t("inbox_empty") : list.length === 1 ? t("inbox_one") : t("inbox_n", { n: list.length })}</div>
      </div>
      <div className="lede">{list.length === 0 ? t("inbox_empty_2") : t("inbox_sub")}</div>
      <div className="inbox-grid" style={{ rowGap: 24 }}>
        {list.map((p) => {
          const c = snap.computers.find((x) => x.host.name === p.host)!;
          const asked = c.feed.steps.find((s) => s.approvalId === p.id);
          return <DecisionCard key={p.id} p={p} host={p.host} agent={agentLabel(c.feed.agent)} askedMs={asked?.startedMs} wide />;
        })}
        {decided.length > 0 && (
          <div>
            <div className="grp-h" style={{ marginTop: 8 }}>
              {t("decided_today")}
              <span className="end c3">{decided.length}</span>
            </div>
            <div className="receipts">
              {decided.map(({ s, host }) => {
                const r = receiptOf(s, host);
                return (
                  <div key={`${host}/${s.key}`} className="rcpt">
                    <span className="ic">{r.you ? <Pointer who="you" h={13} ui /> : <Icon name="clock" size={14} />}</span>
                    <div>
                      <div className="q">
                        {r.q} <span className="c3">· {host}</span>
                      </div>
                      <div className={cx("a", r.you && "you")}>{r.a}</div>
                    </div>
                    <span className="tm">{clock(s.decidedMs ?? s.startedMs, false)}</span>
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </div>
      <div className="footnote" style={{ marginTop: 24 }}>
        <Icon name="info" size={12} />
        <span>{t("inbox_foot")}</span>
      </div>
    </div>
  );
}

function outcomeOf(s: Step): string {
  if (s.state === "waiting") return t("out_waiting");
  if (s.approvalId && s.decision === "approved" && s.state === "done") return t("out_allowed");
  if (s.decision === "denied") return t("out_denied");
  if (s.decision === "timeout" || s.decision === "unavailable") return t("out_timeout");
  if (s.state === "refused") return t("out_refused");
  if (s.state === "running") return t("out_running");
  if (s.state === "failed") return t("out_failed");
  if (s.state === "cancelled") return t("out_cancelled");
  return t("out_done");
}

/** Five evenly spaced clock times across the minimap's range. */
function axisTicks(computers: Computer[], now: number): string[] {
  const starts = computers.flatMap((c) => c.feed.steps.map((s) => s.startedMs));
  const t0 = Math.min(...starts, now);
  return [0, 1, 2, 3, 4].map((i) => clock(t0 + ((now - t0) * i) / 4, false));
}

export function ActivityPage({ snap }: { snap: Snapshot }) {
  const now = useNow(5000);
  const [sel, setSel] = useState<string | null>(null);
  const [listRef, list] = useSize<HTMLDivElement>();
  const rows = useMemo(
    () =>
      snap.computers
        .flatMap((c) => c.feed.steps.map((s) => ({ s, c })))
        .sort((a, b) => b.s.startedMs - a.s.startedMs)
        .slice(0, 300),
    [snap],
  );
  const picked = rows.find((r) => `${r.c.host.name}/${r.s.key}` === sel);
  const mm = snap.computers.filter((c) => c.feed.steps.length > 0);
  const mmW = Math.max((list.w || 600) - 32 - 40 - 32 - 96 - 12, 100);
  return (
    <div className="act">
      <div className="act-list" ref={listRef} style={{ overflowY: "auto" }}>
        <div className="hd">{t("activity")}</div>
        <div className="body c2" style={{ marginTop: 6 }}>
          {t("act_sub")}
        </div>
        {mm.length > 0 && (
          <div className="mm">
            {mm.map((c) => {
              const spec = turnSpec(c.feed, now, c.online !== false);
              return (
                spec && (
                  <div key={c.host.name} className="r">
                    <span className="n">{c.host.name}</span>
                    <TurnLine spec={spec} w={mmW} mini />
                  </div>
                )
              );
            })}
            <div className="ax">
              <span />
              <div className="s">{axisTicks(mm, now).map((x) => <span key={x}>{x}</span>)}</div>
            </div>
          </div>
        )}
        <div className="tbl">
          <div className="th">
            <span>{t("h_time")}</span>
            <span />
            <span>{t("h_what")}</span>
            <span>{t("h_pc")}</span>
            <span>{t("h_tool")}</span>
            <span>{t("h_out")}</span>
          </div>
          <div className="tday">{t("today")}</div>
          {rows.map(({ s, c }) => {
            const key = `${c.host.name}/${s.key}`;
            return (
              <div key={key} className={cx("tr", sel === key && "sel")} onClick={() => setSel(key)} role="button" tabIndex={0}>
                <span className="t">{clock(s.startedMs)}</span>
                <span>
                  <Pointer who={s.byPerson ? "you" : "agent"} h={12} ui />
                </span>
                <span className={cx("s", s.byPerson && "you")}>{s.intent ?? s.tool}</span>
                <span className="c">{c.host.name}</span>
                <span className="k">
                  <span className="mono">{s.tool}</span>
                </span>
                <span className="o">{outcomeOf(s)}</span>
              </div>
            );
          })}
        </div>
      </div>
      <aside className="insp">
        {picked ? (
          <>
            <div className="hdr">
              <span className="w" style={picked.s.approvalId ? undefined : { color: "var(--agent)" }}>
                <Pointer who={picked.s.approvalId || picked.s.byPerson ? "you" : "agent"} h={12} ui />
                {picked.s.approvalId ? t("ins_decision") : t("ins_step")}
              </span>
              <span className="tm">{clock(picked.s.decidedMs ?? picked.s.startedMs)}</span>
            </div>
            <div className="ttl" style={{ marginTop: 12 }}>
              {picked.s.approvalId ? receiptOf(picked.s, picked.c.host.name).a : (picked.s.intent ?? picked.s.tool)}
            </div>
            <div className="body c2" style={{ marginTop: 4 }}>
              {agentLabel(picked.c.feed.agent)} · {picked.c.host.name}
            </div>
            {picked.s.argsPreview && <div className="payload">{picked.s.argsPreview}</div>}
            <dl className="dl">
              {picked.s.approvalId && (
                <>
                  <dt>{t("ins_scope")}</dt>
                  <dd>{t("only_once")}</dd>
                  <dt>{t("ins_by")}</dt>
                  <dd>{picked.s.answeredBy ?? "—"}</dd>
                  <dt>{t("ins_wait")}</dt>
                  <dd>{picked.s.decidedMs ? `${Math.round((picked.s.decidedMs - picked.s.startedMs) / 1000)} s` : "—"}</dd>
                </>
              )}
              {picked.s.intent && (
                <>
                  <dt>{t("ins_says")}</dt>
                  <dd>{picked.s.intent}</dd>
                </>
              )}
              <dt>{t("ins_result")}</dt>
              <dd>
                {outcomeOf(picked.s)}
                {picked.s.durationMs != null && ` · ${picked.s.durationMs} ms`}
              </dd>
              {picked.s.note && (
                <>
                  <dt>{t("s_note")}</dt>
                  <dd>“{picked.s.note}”</dd>
                </>
              )}
            </dl>
          </>
        ) : (
          <div className="body c3">{t("ins_pick")}</div>
        )}
        <div style={{ flex: 1 }} />
        <div className="footnote" style={{ borderTop: "1px solid var(--stroke-1)", paddingTop: 14 }}>
          <span className="hatch" style={{ width: 22, marginTop: 4 }} />
          <span>{t("takeover_not_recorded")}</span>
        </div>
      </aside>
    </div>
  );
}
