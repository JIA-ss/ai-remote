// The strip above the composer while a Codex turn runs, as Codex's own app has it: the turn's plan as
// one pill ("Step 2 / 5" beside a ring of how much is done) that lists every step when hovered. The
// plan is not in the transcript (Codex's app shows it only here). Hidden while a question waits.

import { Circle, CircleCheck, CircleDot } from "lucide-react";
import { useEffect, useState } from "react";
import type { PlanEntry, Turn } from "../../lib/chat";
import { planStep } from "../../lib/codex";
import { t } from "../../lib/i18n";
import { cx } from "../util";

/** The running turn's plan as last updated, if it has one. */
export function livePlan(turns: Turn[]): PlanEntry[] | null {
  const turn = turns[turns.length - 1];
  if (!turn || turn.status !== "running") return null;
  for (let i = turn.blocks.length - 1; i >= 0; i--) {
    const b = turn.blocks[i];
    if (b.kind === "plan") return b.entries.length ? b.entries : null;
  }
  return null;
}

/** How much of the plan is done, as a ring (filled in a moment after it shows). */
function Ring({ done, total }: { done: number; total: number }) {
  const [shown, setShown] = useState(false);
  useEffect(() => {
    const timer = setTimeout(() => setShown(true), 150);
    return () => clearTimeout(timer);
  }, []);
  const r = 5.5;
  const c = 2 * Math.PI * r;
  const part = total ? done / total : 0;
  return (
    <svg className="x-ring" width={14} height={14} viewBox="0 0 14 14" aria-hidden>
      <circle cx={7} cy={7} r={r} className="x-track" />
      <circle cx={7} cy={7} r={r} className="x-done" strokeDasharray={c} strokeDashoffset={shown ? c * (1 - part) : c} />
    </svg>
  );
}

export function CodexLiveStrip({ plan }: { plan: PlanEntry[] }) {
  const [open, setOpen] = useState(false);
  const st = planStep(plan);
  return (
    <div className="cdx-live">
      <div className="cdx-plan-pill" tabIndex={0} aria-label={`${t("cdx_plan")}: ${t("cdx_plan_step", { s: st.step, n: st.total })}`}
        onMouseEnter={() => setOpen(true)} onMouseLeave={() => setOpen(false)} onFocus={() => setOpen(true)} onBlur={() => setOpen(false)}>
        <Ring done={st.done} total={st.total} />
        <span className="tnum">{t("cdx_plan_step", { s: st.step, n: st.total })}</span>
        <div className={cx("cdx-plan-pop", open && "open")} role="tooltip" aria-hidden={!open}>
          <ol>
            {plan.map((e, i) => (
              <li key={i} className={e.status}>
                {e.status === "completed" ? <CircleCheck size={14} aria-hidden /> : e.status === "in_progress" ? <CircleDot size={14} aria-hidden /> : <Circle size={14} aria-hidden />}
                <span>{e.content}</span>
              </li>
            ))}
          </ol>
        </div>
      </div>
    </div>
  );
}
