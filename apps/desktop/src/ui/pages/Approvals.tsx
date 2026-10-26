// The approvals inbox: every request that waits for you, each with a way to its computer, and what
// was decided today folded underneath. The count and the title are in the toolbar.

import { useState } from "react";
import { t } from "../../lib/i18n";
import { detectOs } from "../../lib/platform";
import { allPending, type Snapshot } from "../../lib/store";
import { DecisionCard } from "../Decide";
import { Pointer } from "../glyphs";
import { Icon } from "../Icon";
import { agentLabel, clock, cx } from "../util";
import { receiptOf } from "./common";
import { EmptyState } from "./Empty";

export function ApprovalsPage({ snap, onOpenComputer }: { snap: Snapshot; onOpenComputer?: (host: string) => void }) {
  const [showDecided, setShowDecided] = useState(false);
  const list = allPending(snap);
  const today = new Date().toDateString();
  const decided = snap.computers
    .flatMap((c) => c.feed.steps.filter((s) => s.approvalId && s.state !== "waiting").map((s) => ({ s, host: c.host.name })))
    .filter(({ s }) => new Date(s.decidedMs ?? s.startedMs).toDateString() === today)
    .sort((a, b) => (b.s.decidedMs ?? 0) - (a.s.decidedMs ?? 0));
  // This Mac can deny but not allow: the same for every card, so it is said once, above them.
  const macOnly = detectOs() === "mac";
  return (
    <div className="ipage gl-inbox">
      {list.length === 0 ? (
        <EmptyState icon="shield" title={t("inbox_empty")} body={t("inbox_empty_2")} compact={decided.length > 0} />
      ) : (
        <>
          {macOnly && (
            <div className="gl-note" role="note">
              <Icon name="info" size={16} />
              <span>{t("approval_unavailable_mac")}</span>
            </div>
          )}
          <div className="inbox-grid">
            {list.map((p) => {
              const c = snap.computers.find((x) => x.host.name === p.host)!;
              const asked = c.feed.steps.find((s) => s.approvalId === p.id);
              return (
                <DecisionCard
                  key={p.id}
                  p={p}
                  host={p.host}
                  agent={agentLabel(c.feed.agent)}
                  askedMs={asked?.startedMs}
                  wide
                  macNote={!macOnly}
                  aside={
                    onOpenComputer && (
                      <button type="button" className="gl-open-host" onClick={() => onOpenComputer(p.host)}>
                        {t("open_computer", { host: p.host })}
                        <Icon name="chevR" size={12} />
                      </button>
                    )
                  }
                />
              );
            })}
          </div>
        </>
      )}
      {decided.length > 0 && (
        <div className="gl-decided">
          <button type="button" className="gl-disclose" aria-expanded={showDecided} onClick={() => setShowDecided((v) => !v)}>
            <Icon name="chevR" size={12} className="gl-chev" />
            {t("decided_today")}
            <span className="n">{decided.length}</span>
          </button>
          {showDecided && (
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
          )}
        </div>
      )}
      {list.length > 0 && (
        <div className="footnote gl-foot">
          <Icon name="info" size={12} />
          <span>{t("inbox_foot")}</span>
        </div>
      )}
    </div>
  );
}
