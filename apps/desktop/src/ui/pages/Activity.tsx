// Activity: every step on every computer, newest first, with an inspector for the picked one.

import { useMemo, useState } from "react";
import { t } from "../../lib/i18n";
import type { Computer, Snapshot } from "../../lib/store";
import type { Step } from "../../lib/types";
import { Pointer } from "../glyphs";
import { TurnLine, turnSpec } from "../TurnLine";
import { agentLabel, clock, cx, useNow, useSize } from "../util";
import { receiptOf } from "./common";

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
