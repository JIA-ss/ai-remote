// A Codex question, asked the way Codex's own app asks it: a card in place of the composer, one
// question at a time (with "1 / 2" and arrows when a request has several), options as numbered rows
// (the first chosen by default; a number key, a click or Enter takes one), a row for an answer of the
// reader's own when Codex welcomes one, Skip / Next, and X or Esc to close it — which stops the turn,
// since Codex waits on the answer. All the request's answers go back together once the last question
// is done; a skipped question is left out. What was chosen and typed survives the panel closing.

import { ArrowRight, ChevronLeft, ChevronRight, Pencil, X } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { QuestionBlock } from "../../lib/chat";
import { splitRecommended } from "../../lib/codex";
import { t } from "../../lib/i18n";
import { cx } from "../util";

export type Request = { turn: number; request: string | null; questions: QuestionBlock[] };

/** Where a request's draft is kept: by host, session, turn and request. */
export const requestKey = (host: string, session: string, r: Request) => JSON.stringify([host, session, r.turn, r.request ?? ""]);

type Draft = { i: number; picks: Record<string, number | null>; texts: Record<string, string> };
/** What was chosen and typed so far, per request, while the app runs. */
const drafts = new Map<string, Draft>();

/** Forgets a request's draft (it is over). */
export function dropDraft(id: string) {
  drafts.delete(id);
}

function fresh(questions: QuestionBlock[], had?: Draft): Draft {
  const d: Draft = { i: 0, picks: {}, texts: {} };
  for (const q of questions) {
    d.picks[q.qid] = q.options.length ? 0 : null;
    d.texts[q.qid] = "";
  }
  // A draft is for exactly these questions; anything else starts afresh.
  const same = had && Object.keys(had.picks).length === questions.length && questions.every((q) => q.qid in had.picks);
  return same ? { i: Math.min(had.i, questions.length - 1), picks: { ...d.picks, ...had.picks }, texts: { ...d.texts, ...had.texts } } : d;
}

/** A question's answer: the option chosen, else what was typed, else nothing (skipped). */
function answerOf(q: QuestionBlock, d: Draft): string {
  const k = d.picks[q.qid];
  if (k != null && q.options[k] !== undefined) return q.options[k].trim();
  return (d.texts[q.qid] ?? "").trim();
}

/** "1:45" */
function clock(ms: number): string {
  const s = Math.ceil(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** How long a chosen option shows as chosen before the card moves on. */
const FLASH = 180;
const KEYS = new Set(["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Enter", "Escape"]);

function isField(el: Element | null): boolean {
  return !!el && (el instanceof HTMLTextAreaElement || el instanceof HTMLInputElement || (el as HTMLElement).isContentEditable);
}

export function CodexRequestCard({ req, id, readonly, now, onSubmit, onDismiss }: {
  req: Request;
  /** The draft's key (see `requestKey`). */
  id: string;
  readonly: boolean;
  /** The time now (ticking), for how long until Codex goes on by itself. */
  now: number;
  /** Every question's answer, an empty one for a skipped question. */
  onSubmit: (answers: Array<[string, string]>) => void;
  onDismiss: () => void;
}) {
  const [d, setD] = useState<Draft>(() => fresh(req.questions, drafts.get(id)));
  const [flash, setFlash] = useState<number | null>(null);
  const [otherOn, setOtherOn] = useState(false);
  const card = useRef<HTMLDivElement>(null);
  const field = useRef<HTMLTextAreaElement & HTMLInputElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const moved = useRef(false);
  const n = req.questions.length;
  const q = req.questions[Math.min(d.i, n - 1)];
  const expiresAt = req.questions.find((x) => x.expiresAt !== undefined)?.expiresAt;
  const pick = d.picks[q.qid] ?? null;
  const typed = d.texts[q.qid] ?? "";

  useEffect(() => {
    drafts.set(id, d);
  }, [id, d]);
  useEffect(() => () => clearTimeout(timer.current), []);
  // The card takes the keyboard when it shows, unless the reader is typing (in the composer it
  // replaces, or anywhere else: the next key would answer for them) or on the remote screen; then a
  // click or Tab brings them in.
  // Where the keyboard was as the card first renders, before the composer it replaces is hidden
  // (hiding it moves the focus away, so that cannot be asked afterwards).
  const [before] = useState(() => document.activeElement as HTMLElement | null);
  useEffect(() => {
    const a = before;
    if (a && (a.getAttribute("aria-label") === "Remote screen" || isField(a))) return;
    card.current?.focus({ preventScroll: true });
  }, [id]);
  // The free answer grows with its text, up to 128 px.
  useLayoutEffect(() => {
    const el = field.current;
    if (!el || el.tagName !== "TEXTAREA" || !q.options.length) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 128)}px`;
  }, [typed, q.qid]);

  const submit = (x: Draft) => {
    drafts.delete(id);
    onSubmit(req.questions.map((qq) => [qq.qid, answerOf(qq, x)]));
  };
  /** On to the next question, or (on the last) the answers go. */
  const advance = (x: Draft) => {
    moved.current = true;
    setOtherOn(false);
    if (x.i < n - 1) setD({ ...x, i: x.i + 1 });
    else submit(x);
  };
  const go = (i: number) => {
    moved.current = true;
    setOtherOn(false);
    setD((x) => ({ ...x, i: Math.max(0, Math.min(n - 1, i)) }));
  };
  const select = (k: number) => setD((x) => ({ ...x, picks: { ...x.picks, [q.qid]: k } }));
  const choose = (k: number) => {
    if (readonly || timer.current !== undefined) return;
    const x = { ...d, picks: { ...d.picks, [q.qid]: k } };
    setD(x);
    setFlash(k);
    timer.current = setTimeout(() => {
      timer.current = undefined;
      setFlash(null);
      advance(x);
    }, FLASH);
  };
  const skip = () => advance({ ...d, picks: { ...d.picks, [q.qid]: null }, texts: { ...d.texts, [q.qid]: "" } });
  const next = () => (typed.trim() ? advance({ ...d, picks: { ...d.picks, [q.qid]: null } }) : skip());
  const dismiss = () => {
    clearTimeout(timer.current);
    timer.current = undefined;
    drafts.delete(id);
    onDismiss();
  };
  const focusField = () => {
    setD((x) => ({ ...x, picks: { ...x.picks, [q.qid]: null } }));
    field.current?.focus();
  };

  /** The card's keys; true when one was used. */
  const handle = (e: KeyboardEvent): boolean => {
    if (e.isComposing || e.keyCode === 229) return false;
    if (e.key === "Escape") {
      if (!readonly) dismiss();
      return !readonly;
    }
    // While a choice shows as chosen, nothing else.
    if (readonly || timer.current !== undefined) return false;
    if (isField(e.target as Element)) {
      if (e.key === "Enter" && !e.shiftKey) {
        next();
        return true;
      }
      // From a one-line answer, ↑ goes back to the options.
      if (e.key === "ArrowUp" && q.options.length && !typed.includes("\n")) {
        select(q.options.length - 1);
        card.current?.focus();
        return true;
      }
      return false;
    }
    if (e.metaKey || e.ctrlKey || e.altKey) return false;
    if (/^[1-9]$/.test(e.key)) {
      const k = Number(e.key) - 1;
      if (k < q.options.length) choose(k);
      else if (k === q.options.length && q.other) focusField();
      else return false;
      return true;
    }
    switch (e.key) {
      case "ArrowLeft":
        go(d.i - 1);
        return true;
      case "ArrowRight":
        go(d.i + 1);
        return true;
      case "ArrowDown":
        if (pick === null) {
          if (q.options.length) select(0);
        } else if (pick < q.options.length - 1) select(pick + 1);
        else if (q.other) focusField();
        return true;
      case "ArrowUp":
        if (q.options.length) select(pick === null ? q.options.length - 1 : Math.max(0, pick - 1));
        return true;
      case "Enter":
        if (pick !== null) choose(pick);
        else next();
        return true;
    }
    return false;
  };
  const handleRef = useRef(handle);
  handleRef.current = handle;
  // Keys pressed while the reader is in the conversation itself (its list, not a button, a field or
  // the remote screen) reach the card; not while a menu or a popover is open.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || !KEYS.has(e.key)) return;
      const a = document.activeElement as HTMLElement | null;
      if (!a || card.current?.contains(a) || !a.matches(".nt-chat .nt-chat-body")) return;
      if (document.querySelector(".nt-chat .nt-chat-menu, .nt-chat .nt-usage, .nt-chat .nt-ac, [role=dialog], [role=menu]")) return;
      if (handleRef.current(e)) e.preventDefault();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const desc = (k: number) => (q.descriptions?.[k] ?? "").trim();
  const action = (
    <button type="button" className={cx("btn sm", typed.trim() ? "primary" : "x-outline")} disabled={readonly}
      onKeyDown={(e) => e.key === "Enter" && e.stopPropagation()} onClick={next}>
      {typed.trim() ? t("cdx_q_next_btn") : t("cdx_q_skip")}
    </button>
  );
  const freeField = (rows: number, placeholder: string) => {
    const props = {
      value: typed,
      placeholder,
      disabled: readonly,
      onFocus: () => {
        setOtherOn(true);
        if (q.options.length) setD((x) => ({ ...x, picks: { ...x.picks, [q.qid]: null } }));
      },
      onBlur: () => setOtherOn(false),
      onChange: (e: { target: { value: string } }) =>
        setD((x) => ({ ...x, picks: { ...x.picks, [q.qid]: q.options.length ? null : x.picks[q.qid] }, texts: { ...x.texts, [q.qid]: e.target.value } })),
    };
    return q.secret ? <input ref={field} type="password" autoComplete="off" {...props} placeholder={t("cdx_q_secret_ph")} />
      : <textarea ref={field} rows={rows} {...props} />;
  };
  return (
    <div ref={card} className={cx("cdx-req", readonly && "x-ro")} tabIndex={0} role="group" aria-label={q.prompt}
      onKeyDown={(e) => {
        if (handle(e.nativeEvent)) {
          e.preventDefault();
          e.stopPropagation();
        }
      }}>
      <div className="x-hd">
        <div className="x-ti">{q.prompt}</div>
        {n > 1 && (
          <div className="x-pager tnum">
            <button type="button" className="x-ib" aria-label={t("cdx_q_prev")} disabled={d.i === 0} onClick={() => go(d.i - 1)}><ChevronLeft size={12} /></button>
            <span>{t("cdx_q_page", { i: d.i + 1, n })}</span>
            <button type="button" className="x-ib" aria-label={t("cdx_q_next")} disabled={d.i === n - 1} onClick={() => go(d.i + 1)}><ChevronRight size={12} /></button>
          </div>
        )}
        {!readonly && (
          <button type="button" className="x-ib x-close" aria-label={t("cdx_q_close")} title={t("cdx_q_close")} onClick={dismiss}><X size={14} /></button>
        )}
      </div>
      {/* A question that does not block the turn: Codex goes on by itself when nobody answers. */}
      {expiresAt !== undefined && (
        <div className="x-expires tnum" aria-live="off">{t("cdx_q_expires", { t: clock(Math.max(0, expiresAt - now)) })}</div>
      )}
      {/* A new question fades in; the first one is simply there. */}
      <div className={cx("x-body", moved.current && "x-in")} key={q.qid}>
        {q.options.length > 0 ? (
          <div className="x-opts" role="radiogroup" aria-label={q.prompt}>
            {q.options.map((o, k) => {
              const { label, recommended } = splitRecommended(o);
              return (
                <button key={k} type="button" role="radio" aria-checked={pick === k} disabled={readonly}
                  className={cx("x-opt", pick === k && "on", flash === k && "x-flash")} onClick={() => choose(k)}>
                  <span className="x-mk tnum">{flash === k ? <i className="x-dot" /> : k + 1}</span>
                  <span className="x-lb">
                    <span className="x-l1">
                      {label}
                      {recommended && <span className="x-badge">{t("cdx_q_recommended")}</span>}
                    </span>
                    {desc(k) && <span className="x-desc">{desc(k)}</span>}
                  </span>
                  <ArrowRight size={14} className="x-go" aria-hidden />
                </button>
              );
            })}
            {q.other && (
              <div className={cx("x-opt x-other", (otherOn || typed) && "on")}
                onClick={(e) => !(e.target as HTMLElement).closest("button") && field.current?.focus()}>
                <span className="x-mk"><Pencil size={14} aria-hidden /></span>
                {freeField(1, t("cdx_q_other_ph"))}
                {action}
              </div>
            )}
          </div>
        ) : (
          <div className="x-free">{freeField(4, t("cdx_q_type_here"))}</div>
        )}
        {!(q.options.length > 0 && q.other) && <div className="x-acts">{action}</div>}
      </div>
    </div>
  );
}
