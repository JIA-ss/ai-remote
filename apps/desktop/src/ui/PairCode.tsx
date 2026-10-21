// Pairing with a pair code. The host side: a card with the QR code a phone scans and the eight
// digits a computer types, a countdown, a way to get a new code, and the safety words once a device
// pairs. The controller side: one large field for the digits, with the long ticket behind an
// "advanced" link for machines that cannot reach each other over the network.

import { useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import { t } from "../lib/i18n";
import type { AuditLine, Level, PairTicket } from "../lib/types";
import { Icon } from "./Icon";
import { cx } from "./util";

/** Digits typed or pasted so far, grouped `4821 0937` as they grow. */
export function groupCode(raw: string): string {
  const d = toDigits(raw).slice(0, 8);
  return d.length > 4 ? `${d.slice(0, 4)} ${d.slice(4)}` : d;
}

/** The digits of `raw` (full-width ones too), everything else dropped. */
export function toDigits(raw: string): string {
  return raw.replace(/[０-９]/g, (c) => String.fromCharCode(c.charCodeAt(0) - 0xff10 + 48)).replace(/\D/g, "");
}

/** Whether pasted text is the long ticket rather than a code. */
export function looksLikeTicket(raw: string): boolean {
  return /^\s*(ai-remote\s+pair\s+)?mcpmesh-(invite|enroll):/i.test(raw);
}

/** `m:ss` left until `expiresAt` (epoch seconds). */
function left(expiresAt: number, now: number): string {
  const s = Math.max(0, Math.round(expiresAt - now / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function useNow(ms: number) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), ms);
    return () => clearInterval(timer);
  }, [ms]);
  return now;
}

/** The QR code as one path on a light tile (scanners want dark on light, whatever the theme). */
export function QrTile({ qr, dim, label }: { qr: { size: number; path: string }; dim?: boolean; label: string }) {
  const q = 2; // quiet zone in modules, plus the tile's own padding
  return (
    <div className={cx("nt-qr", dim && "dim")}>
      <svg viewBox={`${-q} ${-q} ${qr.size + 2 * q} ${qr.size + 2 * q}`} shapeRendering="crispEdges" role="img" aria-label={label}>
        <path d={qr.path} fill="#111114" />
      </svg>
    </div>
  );
}

/**
 * The host's pairing dialog: shows a code at once (control level), a new one on request or when
 * the level changes, and the safety words when a device pairs. Closing it withdraws the code.
 */
export function PairCodeDialog({ name, connected, onClose }: { name: string; connected: string[]; onClose: () => void }) {
  const [level, setLevel] = useState<Level>("control");
  const [ticket, setTicket] = useState<PairTicket | null>(null);
  const [since, setSince] = useState(0);
  const [paired, setPaired] = useState<AuditLine | null>(null);
  const [verdict, setVerdict] = useState<"match" | "removed" | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showTicket, setShowTicket] = useState(false);
  const [copied, setCopied] = useState(false);
  const now = useNow(1000);
  const pairedRef = useRef(false);
  pairedRef.current = !!paired;

  const close = () => {
    void api.protectWindow(false).catch(() => {});
    if (!pairedRef.current) void api.localWithdrawCode().catch(() => {});
    onClose();
  };
  useEffect(() => {
    const key = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  });
  useEffect(
    () => () => {
      void api.protectWindow(false).catch(() => {});
    },
    [],
  );

  const make = async (lvl: Level) => {
    setBusy(true);
    setError(null);
    setShowTicket(false);
    try {
      await api.protectWindow(true).catch(() => {});
      setSince(Date.now() - 1000);
      setTicket(await api.localPairCode(lvl));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  // A code right away: opening the dialog is the request.
  useEffect(() => {
    void make(level);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Wait for the device that uses the code, then show the safety words to compare.
  useEffect(() => {
    if (!ticket || paired) return;
    const timer = setInterval(() => {
      api.localRecent(40).then((lines) => {
        const p = lines.filter((l) => l.kind === "paired" && l.ts >= since).at(-1);
        if (p) setPaired(p);
      }, () => {});
    }, 1500);
    return () => clearInterval(timer);
  }, [ticket, paired, since]);

  const code = ticket?.code;
  const expired = !!code && code.expires_at * 1000 <= now;
  const total = 600;
  const frac = code ? Math.max(0, Math.min(1, (code.expires_at - now / 1000) / total)) : 0;
  const peer = paired?.peer ?? "?";

  return (
    <>
      <div className="smoke" style={{ position: "fixed" }} onClick={close} />
      <div className="cdlg nt-pair" role="dialog" aria-modal="true" aria-label={t("pc_title", { name })} style={{ position: "fixed" }}>
        <div className="b">
          {paired ? (
            <div className="nt-pair-done">
              <div className="nt-pair-ok">
                <Icon name="check" size={18} />
              </div>
              <div className="ttl">{t("pc_paired_t", { peer })}</div>
              <div className="bd">{t("pc_paired_d", { level: t(`pc_lvl_${paired.level ?? "control"}` as "pc_lvl_control") })}</div>
              <div className="words md" style={{ marginTop: 20 }}>
                {(paired.sas ?? "").split(/[-\s]+/).join(" ")}
              </div>
              <div className="bd" style={{ marginTop: 14 }}>
                {verdict === "removed" ? t("pc_removed", { peer }) : verdict === "match" ? t("pc_matched", { peer }) : t("pc_compare")}
              </div>
            </div>
          ) : (
            <>
              <div className="ttl">{t("pc_title", { name })}</div>
              <div className="bd">{t("pc_lead")}</div>
              <div className="nt-pair-body">
                {ticket?.qr && code ? (
                  <QrTile qr={ticket.qr} dim={expired} label={t("pc_qr_label", { code: code.grouped })} />
                ) : (
                  <div className={cx("nt-qr", "empty")}>{busy ? <span className="nt-spin" /> : <Icon name="qr" size={28} />}</div>
                )}
                <div className="nt-pair-side">
                  <div className="cap c3">{t("pc_code_h")}</div>
                  <div className={cx("nt-code", expired && "dim")} aria-live="polite">
                    {code ? code.grouped : busy ? "···· ····" : "—"}
                  </div>
                  {code && (
                    <>
                      <div className="nt-ttl-bar" aria-hidden>
                        <span style={{ width: `${(expired ? 0 : frac) * 100}%` }} />
                      </div>
                      <div className="cap c3" style={{ marginTop: 8 }}>
                        {expired ? t("pc_expired") : t("pc_left", { time: left(code.expires_at, now) })}
                      </div>
                    </>
                  )}
                  {ticket && !code && (
                    <div className="cap c-danger" style={{ marginTop: 8 }}>
                      {ticket.code_reason === "lan_discovery_off" ? t("pc_no_code_lan") : t("pc_no_code", { why: ticket.code_error ?? "?" })}
                    </div>
                  )}
                  <div className="nt-pair-ctl">
                    <button className={cx("btn", expired && "primary")} disabled={busy} onClick={() => void make(level)}>
                      <Icon name="refresh" size={14} />
                      {t("pc_new")}
                    </button>
                    <span className="dd-wrap">
                      <select
                        className="dd"
                        aria-label={t("pc_level")}
                        value={level}
                        disabled={busy}
                        onChange={(e) => {
                          const l = e.target.value as Level;
                          setLevel(l);
                          void make(l);
                        }}
                      >
                        <option value="observe">{t("pc_lvl_observe")}</option>
                        <option value="control">{t("pc_lvl_control")}</option>
                        <option value="admin">{t("pc_lvl_admin")}</option>
                      </select>
                      <Icon name="chevD" size={12} />
                    </span>
                  </div>
                </div>
              </div>
              {connected.length > 0 && (
                <div className="cap" style={{ marginTop: 16, color: "var(--relay)" }}>
                  {t("pc_connected", { n: connected.length, names: connected.join(t("list_sep")) })}
                </div>
              )}
              <div className="nt-pair-wait">
                {code && !expired && (
                  <>
                    <span className="nt-pulse" />
                    {t("pc_waiting")}
                  </>
                )}
              </div>
              {ticket && (
                <div className="nt-pair-adv">
                  <button className="nt-link" onClick={() => setShowTicket((v) => !v)} aria-expanded={showTicket}>
                    <Icon name={showTicket ? "chevD" : "chevR"} size={12} />
                    {t("pc_ticket_toggle")}
                  </button>
                  {showTicket && (
                    <div style={{ marginTop: 10 }}>
                      <div className="cap c3" style={{ marginBottom: 8 }}>{t("pc_body")}</div>
                      <div className="nt-ticket mono">{ticket.line}</div>
                      <div className="row" style={{ gap: 8, marginTop: 8, alignItems: "center" }}>
                        <button
                          className="btn"
                          onClick={() => {
                            void navigator.clipboard?.writeText(ticket.line);
                            setCopied(true);
                            setTimeout(() => setCopied(false), 1500);
                          }}
                        >
                          <Icon name={copied ? "check" : "copy"} size={14} />
                          {t("copy")}
                        </button>
                        <span className="cap c3">{t("pc_private")}</span>
                      </div>
                    </div>
                  )}
                </div>
              )}
            </>
          )}
          {error && <div className="cap c-danger" style={{ marginTop: 10 }}>{error}</div>}
        </div>
        <div className="f">
          {paired && !verdict ? (
            <>
              <button className="btn primary" onClick={() => setVerdict("match")}>
                {t("pc_match")}
              </button>
              <button
                className="btn danger"
                disabled={busy}
                onClick={async () => {
                  setBusy(true);
                  try {
                    await api.localForget(peer);
                    setVerdict("removed");
                  } catch (e) {
                    setError(String(e));
                  } finally {
                    setBusy(false);
                  }
                }}
              >
                {t("pc_mismatch")}
              </button>
            </>
          ) : (
            <button className={cx("btn", paired && "primary")} style={{ gridColumn: "1 / -1" }} onClick={close}>
              {t(paired ? "pc_close" : "pc_cancel")}
            </button>
          )}
        </div>
      </div>
    </>
  );
}
