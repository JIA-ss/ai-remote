// Activity: every step on every computer (or the one the toolbar's filter picked), newest first and
// grouped by day, with an inspector for the picked row. The toolbar edge is hard here: a table follows.
// In a narrow window the inspector is a drawer over the list's right side, shown only while a row is
// picked (glass/pages.css); picking the row again, its close button or Escape put it away.

import { Fragment, useMemo, useRef, useState } from "react";
import { currentLang, t } from "../../lib/i18n";
import type { Computer, Snapshot } from "../../lib/store";
import type { Step } from "../../lib/types";
import { Pointer } from "../glyphs";
import { Icon } from "../Icon";
import { TurnLine, turnSpec } from "../TurnLine";
import { agentLabel, clock, cx, useNow, useSize } from "../util";
import { receiptOf } from "./common";
import { EmptyState } from "./Empty";

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

/** A row's day heading: today, yesterday, or the date. */
function dayOf(ms: number, now: number): string {
  const day = new Date(ms).toDateString();
  if (day === new Date(now).toDateString()) return t("today");
  if (day === new Date(now - 86_400_000).toDateString()) return t("day_yesterday");
  const zh = currentLang() === "zh";
  return new Date(ms).toLocaleDateString(zh ? "zh-CN" : "en-US", { month: zh ? "numeric" : "short", day: "numeric" });
}

/** The computers whose steps the page shows: all of them, or the one the toolbar's filter picked. */
export function activityComputers(snap: Snapshot, host?: string): Computer[] {
  return host ? snap.computers.filter((c) => c.host.name === host) : snap.computers;
}

export function ActivityPage({
  snap,
  host,
  onHost,
  onOpenComputer,
}: {
  snap: Snapshot;
  /** Only this computer's steps (the toolbar's filter). */
  host?: string;
  onHost?: (host?: string) => void;
  onOpenComputer?: (host: string) => void;
}) {
  const now = useNow(5000);
  const root = useRef<HTMLDivElement>(null);
  const closeSelection = () => {
    root.current?.querySelector<HTMLElement>(".tr.sel")?.focus({ preventScroll: true });
    setSel(null);
  };
  const [sel, setSel] = useState<string | null>(null);
  const [listRef, list] = useSize<HTMLDivElement>();
  const shown = useMemo(() => activityComputers(snap, host), [snap, host]);
  const rows = useMemo(
    () =>
      shown
        .flatMap((c) => c.feed.steps.map((s) => ({ s, c })))
        .sort((a, b) => b.s.startedMs - a.s.startedMs)
        .slice(0, 300),
    [shown],
  );
  const picked = rows.find((r) => `${r.c.host.name}/${r.s.key}` === sel);
  const mm = shown.filter((c) => c.feed.steps.length > 0);
  const mmW = Math.max((list.w || 600) - 32 - 40 - 32 - 96 - 12, 100);
  if (rows.length === 0)
    return (
      <div className="act gl-act-empty">
        <div className="act-list" ref={listRef}>
          <EmptyState
            icon="history"
            title={host ? t("pg_act_empty_host", { host }) : t("pg_act_empty")}
            body={t("pg_act_empty_d")}
            action={host && onHost && <button type="button" className="btn" onClick={() => onHost(undefined)}>{t("pg_act_show_all")}</button>}
          />
        </div>
      </div>
    );
  const pick = (key: string) => setSel((cur) => (cur === key ? null : key));
  return (
    <div
      className="act" ref={root}
      onKeyDown={(e) => {
        if (e.key !== "Escape" || !sel || e.defaultPrevented) return;
        e.preventDefault();
        closeSelection();
      }}
    >
      <div className="act-list" ref={listRef}>
        <div className="gl-act-lede">{t("act_sub")}</div>
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
          {rows.map(({ s, c }, i) => {
            const key = `${c.host.name}/${s.key}`;
            const day = dayOf(s.startedMs, now);
            const newDay = i === 0 || dayOf(rows[i - 1].s.startedMs, now) !== day;
            return (
              <Fragment key={key}>
                {newDay && <div className="tday">{day}</div>}
                <div
                  className={cx("tr", sel === key && "sel")}
                  onClick={() => pick(key)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      pick(key);
                    }
                  }}
                  role="button"
                  tabIndex={0}
                  aria-pressed={sel === key}
                >
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
              </Fragment>
            );
          })}
        </div>
      </div>
      <aside className="insp" aria-label={t("pg_ins_label")}>
        {picked ? (
          <>
            <button type="button" className="gl-insp-close" onClick={closeSelection} aria-label={t("pg_ins_close")} title={t("pg_ins_close")}>
              <Icon name="x" size={12} />
            </button>
            <div className="hdr">
              <span className="w" style={picked.s.approvalId ? undefined : { color: "var(--agent)" }}>
                <Pointer who={picked.s.approvalId || picked.s.byPerson ? "you" : "agent"} h={12} ui />
                {picked.s.approvalId ? t(picked.s.state === "waiting" ? "out_waiting" : "ins_decision") : t("ins_step")}
              </span>
              <span className="tm">{clock(picked.s.decidedMs ?? picked.s.startedMs)}</span>
            </div>
            <div className="ttl" style={{ marginTop: 12 }}>
              {/* a request still waiting has no decision to state yet: say what was asked */}
              {picked.s.approvalId
                ? picked.s.state === "waiting"
                  ? receiptOf(picked.s, picked.c.host.name).q
                  : receiptOf(picked.s, picked.c.host.name).a
                : (picked.s.intent ?? picked.s.tool)}
            </div>
            <div className="body c2" style={{ marginTop: 4 }}>
              {agentLabel(picked.c.feed.agent)} · {picked.c.host.name}
            </div>
            {onOpenComputer && (
              <button type="button" className="gl-more" onClick={() => onOpenComputer(picked.c.host.name)}>
                {t("open_computer", { host: picked.c.host.name })}
                <Icon name="chevR" size={12} />
              </button>
            )}
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
        <div className="footnote gl-insp-foot">
          <span className="hatch" style={{ width: 22, marginTop: 4 }} />
          <span>{t("takeover_not_recorded")}</span>
        </div>
      </aside>
    </div>
  );
}
