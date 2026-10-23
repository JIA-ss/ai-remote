// The decision card: the one lifted card on a screen, with an amber halo while it waits. It names
// who asks and where (from the pairing record), asks a question, shows the exact payload, attributes
// the reason to the agent, and allows only once. Allowing goes through Windows Hello.

import { useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import { currentLang, t } from "../lib/i18n";
import { detectOs } from "../lib/platform";
import type { PendingApproval } from "../lib/types";
import { Actor, Pointer } from "./glyphs";
import { Icon } from "./Icon";
import { clock, commandOf, countdown, cx, destructive, useNow } from "./util";

type Approver = "unknown" | "none" | "pending" | "ready";

function questionFor(tool: string) {
  if (tool === "shell") return { q: t("ap_q_shell"), verb: t("ap_verb_shell") };
  if (tool === "jobs_start") return { q: t("ap_q_job"), verb: t("ap_verb_job") };
  if (tool === "files_push") return { q: t("ap_q_push"), verb: t("ap_verb_push") };
  if (tool === "files_pull") return { q: t("ap_q_pull"), verb: t("ap_verb_pull") };
  if (tool === "files_delete") return { q: t("ap_q_delete"), verb: t("ap_verb_delete") };
  if (tool === "files_mkdir") return { q: t("ap_q_mkdir"), verb: t("ap_verb_mkdir") };
  return { q: t("ap_q_tool", { tool }), verb: t("ap_verb_tool") };
}

/** The payload, with the verb of a destructive command in red. */
function Payload({ text }: { text: string }) {
  const m = /^(\S+)([\s\S]*)$/.exec(text.trim());
  if (m && destructive(m[1])) {
    return (
      <div className="payload">
        <span className="verb">{m[1]}</span>
        {m[2]}
      </div>
    );
  }
  return <div className="payload">{text}</div>;
}

export function DecisionCard({
  p,
  host,
  agent,
  wide = false,
  askedMs,
  onDone,
}: {
  p: PendingApproval;
  host: string;
  agent: string;
  wide?: boolean;
  /** When the request arrived (from the feed), shown in the header. */
  askedMs?: number;
  onDone?: (verdict: "allowed" | "denied") => void;
}) {
  const now = useNow(1000);
  const zh = currentLang() === "zh";
  const canApprove = detectOs() !== "mac";
  const [approver, setApprover] = useState<Approver>("unknown");
  const [busy, setBusy] = useState<null | "allow" | "deny" | "setup">(null);
  const [error, setError] = useState<string | null>(null);
  const [telling, setTelling] = useState(false);
  const [note, setNote] = useState("");
  const noteRef = useRef<HTMLTextAreaElement>(null);
  const cardRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    api
      .approverStatus(host)
      .then((s) => live && setApprover(s === true ? "ready" : s === false ? "pending" : "none"))
      .catch(() => live && setApprover("none"));
    return () => {
      live = false;
    };
  }, [host]);

  useEffect(() => {
    if (telling) noteRef.current?.focus();
  }, [telling]);

  const run = async (kind: "allow" | "deny" | "setup", f: () => Promise<void>) => {
    setBusy(kind);
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(null);
    }
  };

  const allow = () =>
    run("allow", async () => {
      await api.approve(host, p.id);
      onDone?.("allowed");
    });
  const deny = (withNote?: string) =>
    run("deny", async () => {
      await api.deny(host, p.id, withNote);
      onDone?.("denied");
    });
  const setup = () =>
    run("setup", async () => {
      setApprover("pending");
      await api.enrollApprover(host);
      setApprover("ready");
    });

  // Esc belongs to the focused card, or the first card when focus is elsewhere. A single key
  // press must never deny a whole queue, nor escape from an open dialog or menu to a card behind it.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented || e.repeat || e.isComposing || e.keyCode === 229) return;
      if (e.target instanceof Element && e.target.closest(".nt-picture")) return;
      if (document.documentElement.hasAttribute("data-modal") || document.querySelector('[aria-modal="true"], [role="menu"], [role="listbox"]')) return;
      const focused = document.activeElement?.closest(".decide");
      if ((focused ?? document.querySelector(".decide")) !== cardRef.current || telling || busy) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      void deny();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const command = commandOf(p.argsPreview);
  const { q, verb } = questionFor(p.tool);
  const danger = destructive(command);
  const verbBtn = (
    <button
      className={cx("btn", danger ? "danger" : "primary")}
      disabled={!canApprove || approver !== "ready" || busy !== null}
      onClick={allow}
      title={approver === "ready" ? t("hello_first") : undefined}
    >
      {busy === "allow" ? t("approving") : verb}
    </button>
  );
  const denyBtn = (
    <button className="btn" disabled={busy !== null} onClick={() => void deny()}>
      {t("deny")}
      <span className="kbd">Esc</span>
    </button>
  );
  const tell = (
    <button className="btn subtle tellr" onClick={() => setTelling(true)}>
      <Icon name="msg" size={14} />
      {t("tell_instead")}
    </button>
  );

  return (
    <div className="decide" ref={cardRef}>
      <div className="hdr">
        <span className="w">
          <Pointer who="you" h={12} ui />
          {t("waiting_for_you")}
        </span>
        {askedMs && <span className="tm">{clock(askedMs)}</span>}
      </div>
      <div className="who">
        <Actor who="agent" h={11} />
        <span>{t("wants_tool", { agent, tool: p.tool, host })}</span>
      </div>
      <div className="q hd">{q}</div>
      {p.intent && (
        <div className="says">
          <div className="l">{t("agent_says_l")}</div>
          <div className="v">{p.intent}</div>
        </div>
      )}
      <Payload text={command} />
      {!canApprove && <div className="prov"><Icon name="info" size={16} /><span>{t("approval_unavailable_mac")}</span></div>}
      {canApprove && approver === "none" && (
        <div className="prov">
          <Icon name="info" size={16} />
          <span>
            {t("setup_approver_body", { host })}
            <div style={{ marginTop: 10 }}>
              <button className="btn" disabled={busy !== null} onClick={setup}>
                {t("setup_approver")}
              </button>
            </div>
          </span>
        </div>
      )}
      {approver === "pending" && (
        <div className="conseq info">
          <Icon name="clock" size={16} />
          <span>{t("awaiting_accept", { host })}</span>
        </div>
      )}
      {error && (
        <div className="conseq">
          <Icon name="warn" size={16} />
          <span>{error}</span>
        </div>
      )}
      {telling ? (
        <div style={{ marginTop: 14 }}>
          <textarea
            ref={noteRef}
            className="ta-input"
            rows={3}
            value={note}
            placeholder={t("composer_decide")}
            onChange={(e) => setNote(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && note.trim()) {
                e.preventDefault();
                void deny(note.trim());
              }
              if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                setTelling(false);
              }
            }}
          />
          <div className="acts">
            <button className="btn primary" disabled={!note.trim() || busy !== null} onClick={() => void deny(note.trim())}>
              {t("send")}
            </button>
            <button className="btn" onClick={() => setTelling(false)}>
              {t("back")}
            </button>
          </div>
        </div>
      ) : wide ? (
        <div className="acts wide">
          {verbBtn}
          {denyBtn}
          {tell}
        </div>
      ) : (
        <>
          <div className="acts">
            {verbBtn}
            {denyBtn}
          </div>
          <div className="tell">{tell}</div>
        </>
      )}
      <div className="ft">
        <b>{t("only_once")}</b>
        <span>{t("denied_in", { time: countdown(p.expiresMs, now, zh) })}</span>
      </div>
    </div>
  );
}
