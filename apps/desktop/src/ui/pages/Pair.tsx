// Pairing: the ticket, then the safety words both screens show.

import { useState } from "react";
import { api, type Paired } from "../../lib/api";
import { t } from "../../lib/i18n";
import { CmdLine } from "../CmdLine";
import { SeatMark } from "../glyphs";
import { Icon } from "../Icon";
import { groupCode, looksLikeTicket, toDigits } from "../PairCode";
import { cx } from "../util";
import { levelName } from "./common";

/** Pairing: the ticket, then the safety words both screens show. */
export function PairPage({ onPaired }: { onPaired: (host: string) => void }) {
  const [code, setCode] = useState("");
  const [ticket, setTicket] = useState("");
  const [useTicket, setUseTicket] = useState(false);
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [paired, setPaired] = useState<Paired | null>(null);
  const [step, setStep] = useState<1 | 2 | 3 | "mismatch">(1);
  const host = paired?.peer ?? (name.trim() || "—");
  const ready = useTicket ? !!ticket.trim() : toDigits(code).length === 8;

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
          <div className="hd">{t(useTicket ? "pr_use_ticket" : "pr_door1")}</div>
          <div className="body c2" style={{ marginTop: 8 }}>
            {t(useTicket ? "pr_ticket_d" : "pr_door1_d")}
          </div>
          {useTicket ? (
            <textarea
              className="ta-input mono"
              rows={4}
              style={{ marginTop: 20 }}
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
              <div className="cap c3" style={{ marginTop: 8 }}>
                {t("pr_code_hint")}
              </div>
            </>
          )}
          <div className="body" style={{ marginTop: 22 }}>
            {t("pr_name")}
          </div>
          <input className="field lg ph-field" value={name} onChange={(e) => setName(e.target.value)} placeholder={t("pr_name_ph")} style={{ marginTop: 6, width: "100%" }} />
          {error && (
            <div className="conseq" style={{ marginTop: 14 }}>
              <Icon name="warn" size={16} />
              <span>{error}</span>
            </div>
          )}
          <div className="row" style={{ marginTop: 20, gap: 16, alignItems: "center" }}>
            <button className="btn primary lg" disabled={busy || !ready} onClick={() => void pair()}>
              {busy ? t(useTicket ? "pairing" : "pairing_code") : t("pair_btn")}
            </button>
            <button
              className="nt-link"
              disabled={busy}
              onClick={() => {
                setUseTicket((v) => !v);
                setError(null);
              }}
            >
              {t(useTicket ? "pr_use_code" : "pr_use_ticket")}
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
