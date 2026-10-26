// Pairing: the code (or a ticket), then the safety words both screens show, then done. One centred
// column; Cancel and the toolbar's ‹ go back; leaving after pairing but before the words are checked
// asks first, because the new pairing is then removed. The other way round (this computer shows a
// code) is one button away at the bottom.

import { useContext, useEffect, useState } from "react";
import type { NavGuard } from "../../lib/nav";
import { api, type Paired } from "../../lib/api";
import { t } from "../../lib/i18n";
import { Confirm } from "../ComputerTabs";
import { SeatMark } from "../glyphs";
import { Icon } from "../Icon";
import { groupCode, looksLikeTicket, toDigits } from "../PairCode";
import { ShellContext } from "../Shell";
import { cx } from "../util";
import { levelName } from "./common";

/** Pairing: the ticket, then the safety words both screens show. */
export function PairPage({
  onPaired,
  onShowMyCode,
  registerGuard,
  onAgents,
}: {
  onPaired: (host: string) => void;
  /** Settings › This device with its pair-code dialog open (pairing the other way round). */
  onShowMyCode?: () => void;
  /** Asks before the shell navigates away (step 2: paired, safety words not checked yet). */
  registerGuard?: (guard: NavGuard | null) => void;
  /** Settings › Agent access (how to connect Claude Code, Codex, ...); a text hint without it. */
  onAgents?: () => void;
}) {
  const shell = useContext(ShellContext);
  const [code, setCode] = useState("");
  const [ticket, setTicket] = useState("");
  const [useTicket, setUseTicket] = useState(false);
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [paired, setPaired] = useState<Paired | null>(null);
  const [step, setStep] = useState<1 | 2 | 3 | "mismatch">(1);
  // The navigation held by the guard while "Leave pairing?" is open.
  const [leaving, setLeaving] = useState<(() => void) | null>(null);
  const host = paired?.peer ?? (name.trim() || "—");
  const ready = useTicket ? !!ticket.trim() : toDigits(code).length === 8;

  // Paired but the words not checked: leaving removes the pairing, so ask first.
  const unchecked = step === 2 && !!paired;
  useEffect(() => {
    if (!registerGuard) return;
    registerGuard(unchecked ? (proceed) => (setLeaving(() => proceed), true) : null);
    return () => registerGuard(null);
  }, [unchecked, registerGuard]);

  const pair = async () => {
    if (!ready || busy) return;
    setBusy(true);
    setError(null);
    try {
      const what = useTicket ? ticket.trim().replace(/^ai-remote\s+pair\s+/, "") : toDigits(code);
      const p = await api.pair(what, name.trim() || undefined);
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
    <div className="ppage">
      <div className="gl-pair">
        <div className="steps4" aria-label={t("step_of", { n: cur, m: 3 })}>
          {steps.map(([n, label], i) => (
            <span key={n} style={{ display: "contents" }}>
              {i > 0 && <span className="bar" />}
              <span className={cx("s", n < cur && "done", n === cur && "cur")} aria-current={n === cur ? "step" : undefined}>
                <span className="n">{n < cur ? <Icon name="check" size={10} /> : n}</span>
                {label}
              </span>
            </span>
          ))}
        </div>

        {step === 1 && (
          <div className="gl-pair-step">
            <div className="hd">{t(useTicket ? "pr_use_ticket" : "pr_door1")}</div>
            <div className="body c2 gl-pair-d">{t(useTicket ? "pr_ticket_d" : "pr_door1_d")}</div>
            {useTicket ? (
              <textarea
                className="ta-input mono gl-pair-ticket"
                rows={4}
                value={ticket}
                autoFocus
                onChange={(e) => setTicket(e.target.value)}
                placeholder={t("pr_ticket_ph")}
              />
            ) : (
              <>
                <input
                  className="nt-code-input"
                  aria-label={t("pr_code_label")}
                  inputMode="numeric"
                  autoComplete="one-time-code"
                  autoFocus
                  spellCheck={false}
                  value={code}
                  placeholder={t("pr_code_ph")}
                  onChange={(e) => {
                    const v = e.target.value;
                    // A pasted ticket goes to the ticket field.
                    if (looksLikeTicket(v)) {
                      setTicket(v);
                      setUseTicket(true);
                      return;
                    }
                    setCode(groupCode(v));
                  }}
                  onKeyDown={(e) => e.key === "Enter" && void pair()}
                />
                <div className="cap c3 gl-pair-hint">{t("pr_code_hint")}</div>
              </>
            )}
            <label className="gl-pair-name">
              <span className="body">{t("pr_name")}</span>
              <input className="field lg ph-field" value={name} onChange={(e) => setName(e.target.value)} placeholder={t("pr_name_ph")} />
            </label>
            {error && (
              <div className="conseq">
                <Icon name="warn" size={16} />
                <span>{error}</span>
              </div>
            )}
            <div className="gl-pair-acts">
              <button className="btn primary lg" disabled={busy || !ready} onClick={() => void pair()}>
                {busy ? t(useTicket ? "pairing" : "pairing_code") : t("pair_btn")}
              </button>
              {shell && (
                <button className="btn lg" disabled={busy} onClick={shell.back}>
                  {t("cancel")}
                </button>
              )}
              <button
                className="nt-link gl-pair-alt"
                disabled={busy}
                onClick={() => {
                  setUseTicket((v) => !v);
                  setError(null);
                }}
              >
                {t(useTicket ? "pr_use_code" : "pr_use_ticket")}
              </button>
            </div>
            {onShowMyCode && (
              <div className="gl-pair-rev">
                <span>{t("pair_others_q")}</span>
                <button className="btn" onClick={onShowMyCode}>
                  {t("pair_show_mine")}
                </button>
              </div>
            )}
          </div>
        )}

        {step === 2 && paired && (
          <div className="gl-pair-step">
            <div className="hd">{t("pr_words_q", { host })}</div>
            <div className="body c2 gl-pair-d">{t("pr_words_d")}</div>
            <div className="words gl-pair-words">{paired.sas.split(/[-\s]+/).join(" ")}</div>
            <div className="gl-pair-acts">
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
            <div className="cap c2 gl-pair-note">{t("pr_mismatch_note", { host })}</div>
          </div>
        )}

        {step === "mismatch" && (
          <div className="gl-pair-step">
            <div className="hd">{t("pr_err_words")}</div>
            <div className="body c2 gl-pair-d">{t("pg_err_words_d", { host })}</div>
            <div className="gl-pair-acts">
              <button
                className="btn lg"
                onClick={() => {
                  setPaired(null);
                  setTicket("");
                  setStep(1);
                }}
              >
                {t("start_over")}
              </button>
            </div>
          </div>
        )}

        {step === 3 && paired && (
          <div className="gl-pair-step">
            <div className="gl-pair-done">
              <SeatMark state="both" h={32} />
              <div className="hd">{t("pr_done", { host })}</div>
            </div>
            <div className="body c2 gl-pair-d">{t("pg_done_d", { level: levelName(paired.level) })}</div>
            <div className="gl-pair-acts">
              <button className="btn primary lg" onClick={() => onPaired(paired.peer)}>
                {t("open_computer", { host })}
              </button>
              {shell && (
                <button className="btn lg" onClick={shell.back}>
                  {t("pg_done")}
                </button>
              )}
              {onAgents && (
                <button className="nt-link gl-pair-alt" onClick={onAgents}>
                  {t("pg_agents_go")}
                </button>
              )}
            </div>
          </div>
        )}
      </div>
      {leaving && paired && (
        <Confirm
          title={t("pair_leave_title")}
          body={t("pair_leave_body")}
          verb={t("pair_leave_verb")}
          danger
          onNo={() => setLeaving(null)}
          onYes={async () => {
            await api.unpair(paired.peer).catch(() => {});
            const go = leaving;
            setLeaving(null);
            go();
          }}
        />
      )}
    </div>
  );
}
