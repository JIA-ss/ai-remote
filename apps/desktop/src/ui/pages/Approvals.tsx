// The approvals inbox: every request that waits for you, and what was decided today.

import { t } from "../../lib/i18n";
import { allPending, type Snapshot } from "../../lib/store";
import { DecisionCard } from "../Decide";
import { Pointer, SeatMark } from "../glyphs";
import { Icon } from "../Icon";
import { agentLabel, clock, cx } from "../util";
import { receiptOf } from "./common";

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
