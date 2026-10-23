// What the agent waits on the person for, docked above the composer (TC-35 … TC-54): an approval
// ("Allow Claude to run …?"), a plan to accept or send back, and a question of one or more parts. Only
// the oldest open one shows; up to two more peek out behind it. Keys follow lib/claude.ts `approvalKey`:
// Esc denies, ⌘/Ctrl+↵ allows once (unless the composer has a draft), ⌘/Ctrl+⇧+↵ allows for the
// session; nothing in the first 400 ms after a card appears (a card that pops up under a typing hand).

import { ChevronDown, ChevronLeft, ChevronRight, Check } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { openRequests, type QuestionBlock, type Request } from "../../lib/chat";
import {
  approvalKey,
  describe,
  ARM_MS,
  permitView,
  qAnswered,
  qAnswers,
  qInit,
  qNext,
  qOther,
  qPick,
  qSkip,
  questionParts,
  questionSelectionBlocks,
  type CardKey,
  type QDraft,
} from "../../lib/claude";
import { currentLang, t, type Key } from "../../lib/i18n";
import { inlineText, parseInline } from "../../lib/markdown";
import { detectOs } from "../../lib/platform";
import { cx } from "../util";
import { InlineMarkdown, Markdown } from "../chat/Markdown";
import { renderAnsiText } from "../chat/Ansi";
import { DiffView } from "./ToolBody";
import { workspaceOwnsEvent } from "../workspace/parts";

const MAC = detectOs() === "mac";
const CMD_ENTER = MAC ? "⌘↵" : "Ctrl+↵";
const CMD_SHIFT_ENTER = MAC ? "⌘⇧↵" : "Ctrl+Shift+↵";

export type OpenItem = ReturnType<typeof openRequests>[number];
type Permission = Extract<Request, { kind: "permission" }>;
type Plan = Extract<Request, { kind: "plan" }>;
type Question = QuestionBlock & { turn: number };

export interface CardActions {
  /** `message`: with a denial, what the agent is told instead (the turn goes on). */
  permit: (r: Permission, decision: "once" | "session" | "deny", message?: string) => void;
  plan: (r: Plan, accept: { mode: string } | { revise: string }) => void;
  answer: (q: Question, answers: Record<string, string>) => void;
}

/** Where the keyboard focus is, for a card's keys. */
function focusOf(card: HTMLElement | null): CardKey["focus"] {
  const a = document.activeElement as HTMLElement | null;
  if (!a || a === document.body) return "other";
  if (a.matches("textarea, input, select, [contenteditable]")) {
    if (a.closest(".nt-compose")) return "composer";
    return "field";
  }
  if (a.closest("[role=menu], [role=listbox]")) return "field";
  if (card?.contains(a)) return "card";
  return "other";
}

const itemId = (it: OpenItem) => ("kind" in it && it.kind !== "question" ? it.id : (it as Question).qid);

/** The card in front and the ones waiting behind it. */
export function CardDock({ chat, agent, readonly, draft, actions }: {
  chat: Parameters<typeof openRequests>[0];
  agent: string;
  readonly: boolean;
  /** The composer has something to send. */
  draft: boolean;
  actions: CardActions;
}) {
  const items = openRequests(chat);
  if (!items.length) return null;
  const front = items[0];
  const behind = Math.min(2, items.length - 1);
  return (
    <div className="cl-dock-cards" data-behind={behind}>
      {Array.from({ length: behind }, (_, i) => <div key={i} className="cl-ghost" style={{ ["--n" as string]: i + 1 }} aria-hidden />)}
      <FrontCard key={itemId(front)} item={front} agent={agent} readonly={readonly} draft={draft} actions={actions} />
    </div>
  );
}

function FrontCard({ item, agent, readonly, draft, actions }: { item: OpenItem; agent: string; readonly: boolean; draft: boolean; actions: CardActions }) {
  const box = useRef<HTMLDivElement>(null);
  const armedAt = useRef(Date.now());
  const armed = () => Date.now() - armedAt.current >= ARM_MS;
  if ("kind" in item && item.kind === "permission") return <ApprovalCard r={item} box={box} agent={agent} readonly={readonly} draft={draft} armed={armed} armedAt={armedAt} actions={actions} />;
  if ("kind" in item && item.kind === "plan") return <PlanCard r={item} box={box} agent={agent} readonly={readonly} draft={draft} armed={armed} actions={actions} />;
  return <QuestionCard q={item as Question} box={box} readonly={readonly} armed={armed} actions={actions} />;
}

type CardProps = { box: React.RefObject<HTMLDivElement | null>; agent: string; readonly: boolean; draft: boolean; armed: () => boolean };

/** Keys for the card in front (before the composer's). A workspace beside the live view owns only
 *  its own events: remote keyboard input must never approve or dismiss an agent's card. */
function useCardKeys(handler: (e: KeyboardEvent) => boolean, on: boolean, box: React.RefObject<HTMLElement | null>) {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => {
    if (!on) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      const pane = box.current?.closest<HTMLElement>('.ws[data-embedded="true"]');
      if (pane && !workspaceOwnsEvent(pane, e)) return;
      if (ref.current(e)) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [on, box]);
}

function Kbd({ k }: { k: string }) {
  return <kbd className="cl-kbd">{k}</kbd>;
}

// ---------------------------------------------------------------------------------------------
// Approval

function ApprovalCard({ r, box, agent, readonly, draft, armed, armedAt, actions }: CardProps & { r: Permission; armedAt: React.RefObject<number>; actions: CardActions }) {
  const v = permitView(r);
  const [noting, setNoting] = useState(false);
  const [note, setNote] = useState("");
  const answer = (d: "once" | "session" | "deny") => {
    if (readonly || !armed()) return;
    actions.permit(r, d);
  };
  const denyWithNote = () => {
    if (readonly || !armed()) return;
    actions.permit(r, "deny", note);
  };
  useCardKeys((e) => {
    if (noting) {
      if (e.key !== "Escape" || e.isComposing || e.metaKey || e.ctrlKey) return false;
      setNoting(false);
      return true;
    }
    const act = approvalKey({ key: e.key, meta: e.metaKey, ctrl: e.ctrlKey, shift: e.shiftKey, alt: e.altKey, repeat: e.repeat, composing: e.isComposing, now: Date.now(), focus: focusOf(box.current) },
      { armedAt: armedAt.current, draft, session: v.session, buttons: v.session ? 3 : 2 });
    if (!act) return false;
    if (act.act === "swallow") return true;
    if (act.act === "button") answer(([`deny`, v.session ? "session" : "once", "once"] as const)[act.index]);
    else answer(act.act);
    return true;
  }, !readonly, box);
  const action = t(`cl_ap_a_${v.action}` as Key);
  // "Run the test suite" reads as the action itself in English: "Allow Claude to run the test suite?"
  const own = v.action === "run" && v.description && currentLang() === "en" ? describe(v.description, "failed") : null;
  const title = own?.known ? t("cl_ap_title_bare", { agent, action: own.text })
    : v.object ? t("cl_ap_title", { agent, action, obj: v.object }) : t("cl_ap_title_bare", { agent, action });
  return (
    <div ref={box} className="cl-card cl-ap" role="group" aria-label={title}>
      <div className="cl-card-scroll" aria-label={t("cl_ap_details")}>
        <div className="ttl" title={v.path}>{title}</div>
        {v.command && v.command !== v.object && <pre className="cl-ap-cmd">{v.command}</pre>}
        {(v.background || v.unsandboxed) && (
          <div className="cl-ap-note">{[v.background ? t("cl_ap_bg") : "", v.unsandboxed ? t("cl_ap_unsandboxed") : ""].filter(Boolean).join(" ")}</div>
        )}
        {v.description && v.description !== v.object && (
          <div className="cl-ap-row"><span className="k">{t("cl_ap_desc")}</span><span>{v.description}</span></div>
        )}
        {v.reason && <div className="cl-ap-row"><span className="k">{t("cl_ap_reason")}</span><span>{v.reason}</span></div>}
        {v.diff && <DiffView c={{ type: "diff", path: v.diff.path, old: v.diff.old, new: v.diff.new }} head={false} />}
      </div>
      {noting && (
        <textarea className="cl-pl-note cl-ap-note-field" rows={3} autoFocus value={note} placeholder={t("cl_ap_note_ph", { agent })}
          onChange={(e) => setNote(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              denyWithNote();
            }
          }} />
      )}
      {readonly && <div className="cl-card-note">{t("cl_card_readonly")}</div>}
      <div className="cl-btns">
        <div className="l">
          {noting ? (
            <button type="button" className="cl-btn" disabled={readonly} onClick={() => setNoting(false)}>
              {t("cl_pl_back")}
              <Kbd k="esc" />
            </button>
          ) : (
            <>
              <button type="button" className="cl-btn" disabled={readonly} onMouseDown={(e) => e.preventDefault()} onClick={() => answer("deny")}>
                {t("cl_ap_deny")}
                <Kbd k="esc" />
              </button>
              <button type="button" className="cl-btn" disabled={readonly} onMouseDown={(e) => e.preventDefault()} onClick={() => setNoting(true)}>
                {t("cl_ap_deny_note")}
              </button>
            </>
          )}
        </div>
        <div className="r">
          {noting ? (
            <button type="button" className="cl-btn primary" disabled={readonly || !note.trim()} onClick={denyWithNote}>{t("cl_ap_deny_send")}</button>
          ) : v.session && (
            <button type="button" className="cl-btn" disabled={readonly} title={t("cl_ap_session_tip")} onMouseDown={(e) => e.preventDefault()} onClick={() => answer("session")}>
              {t("cl_ap_session")}
            </button>
          )}
          {!noting && (
            <button type="button" className="cl-btn primary" disabled={readonly} onMouseDown={(e) => e.preventDefault()} onClick={() => answer("once")}>
              {t("cl_ap_once")}
              {!draft && <Kbd k={CMD_ENTER} />}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Plan

const ACCEPT_MODES = ["acceptEdits", "default", "bypassPermissions"] as const;

function PlanCard({ r, box, agent, readonly, draft, armed, actions }: CardProps & { r: Plan; actions: CardActions }) {
  const [shown, setShown] = useState(true);
  const [revising, setRevising] = useState(false);
  const [note, setNote] = useState("");
  const [menu, setMenu] = useState(false);
  const accept = (mode: string) => {
    if (readonly || !armed()) return;
    actions.plan(r, { mode });
  };
  const revise = () => {
    if (readonly || !armed()) return;
    actions.plan(r, { revise: note });
  };
  useCardKeys((e) => {
    if (e.isComposing || e.altKey) return false;
    const focus = focusOf(box.current);
    if (e.key === "Escape" && !e.metaKey && !e.ctrlKey) {
      if (menu) setMenu(false);
      else if (focus === "field" && !revising) return false;
      else setRevising((v) => !v);
      return true;
    }
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && !revising && focus !== "field") {
      if (!e.shiftKey && draft) return false;
      if (e.repeat || !armed()) return true;
      accept(e.shiftKey ? "default" : "acceptEdits");
      return true;
    }
    return false;
  }, !readonly, box);
  return (
    <div ref={box} className="cl-card cl-pl" role="group" aria-label={t("cl_pl_title", { agent })}
      onDragOver={(e) => { e.preventDefault(); e.dataTransfer.dropEffect = "none"; }}>
      <div className="hd">
        <span className="ttl">{t("cl_pl_title", { agent })}</span>
        <button type="button" className="cl-link" onClick={() => setShown((v) => !v)} aria-expanded={shown}>
          {shown ? t("cl_pl_hide") : t("cl_pl_show")}
        </button>
      </div>
      {shown && (
        <div className="cl-card-scroll cl-pl-body">
          <Markdown text={r.plan} className="cl-md" renderAnsi={renderAnsiText} />
        </div>
      )}
      {revising && (
        <textarea className="cl-pl-note" rows={3} autoFocus value={note} placeholder={t("cl_pl_ph")} onChange={(e) => setNote(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              revise();
            }
          }} />
      )}
      {readonly && <div className="cl-card-note">{t("cl_card_readonly")}</div>}
      <div className="cl-btns">
        <div className="l">
          {revising ? (
            <button type="button" className="cl-btn" disabled={readonly} onClick={() => setRevising(false)}>{t("cl_pl_back")}</button>
          ) : (
            <>
              <button type="button" className="cl-btn" disabled={readonly} onMouseDown={(e) => e.preventDefault()} onClick={revise}>{t("cl_pl_reject")}</button>
              <button type="button" className="cl-btn" disabled={readonly} onMouseDown={(e) => e.preventDefault()} onClick={() => setRevising(true)}>
                {t("cl_pl_revise")}
                <Kbd k="esc" />
              </button>
            </>
          )}
        </div>
        <div className="r">
          {revising ? (
            <button type="button" className="cl-btn primary" disabled={readonly} onClick={revise}>{t("cl_pl_send")}</button>
          ) : (
            <>
              <button type="button" className="cl-btn" disabled={readonly} onMouseDown={(e) => e.preventDefault()} onClick={() => accept("default")}>
                {t("cl_pl_accept_default")}
              </button>
              <span className="cl-split">
                <button type="button" className="cl-btn primary" disabled={readonly} onMouseDown={(e) => e.preventDefault()} onClick={() => accept("acceptEdits")}>
                  {t("cl_pl_accept_acceptEdits")}
                  {!draft && <Kbd k={CMD_ENTER} />}
                </button>
                <button type="button" className="cl-btn primary more" disabled={readonly} aria-label={t("cl_pl_modes")} aria-expanded={menu} onClick={() => setMenu((v) => !v)}>
                  <ChevronDown size={13} aria-hidden />
                </button>
                {menu && (
                  <div className="cl-menu up" role="menu">
                    {ACCEPT_MODES.map((m) => (
                      <button key={m} type="button" role="menuitem" onClick={() => { setMenu(false); accept(m); }}>
                        {t(`cl_pl_accept_${m}` as Key)}
                        {m === "default" && <Kbd k={CMD_SHIFT_ENTER} />}
                      </button>
                    ))}
                  </div>
                )}
              </span>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Question

const DRAFTS = new Map<string, QDraft>();

function QuestionCard({ q, box, readonly, armed, actions }: Omit<CardProps, "agent" | "draft"> & { q: Question; actions: CardActions }) {
  const parts = questionParts(q);
  const [d, setD] = useState<QDraft>(() => DRAFTS.get(q.qid) ?? qInit(parts.length));
  useEffect(() => {
    DRAFTS.set(q.qid, d);
  }, [d]);
  const i = Math.min(d.page, parts.length - 1);
  const part = parts[i];
  const multi = !!part.multi;
  const last = i === parts.length - 1;
  const submit = (draft: QDraft) => {
    DRAFTS.delete(q.qid);
    actions.answer(q, qAnswers(draft, parts));
  };
  const primary = () => {
    if (readonly || !armed()) return;
    const r = qNext(d, parts);
    if (r.submit) submit(r.draft);
    else setD(r.draft);
  };
  const skip = () => {
    if (readonly || !armed()) return;
    const r = qSkip(d, parts);
    if (r.submit) submit(r.draft);
    else setD(r.draft);
  };
  useCardKeys((e) => {
    if (e.key !== "Enter" || !(e.metaKey || e.ctrlKey) || e.isComposing || e.altKey) return false;
    if (focusOf(box.current) === "composer") return false;
    if (e.repeat || !armed()) return true;
    primary();
    return true;
  }, !readonly, box);
  const other = d.other[i];
  const answered = qAnswered(d, i);
  const open = parts.findIndex((_, k) => !qAnswered(d, k));
  const questionLabel = inlineText(parseInline(part.question));
  return (
    <div ref={box} className="cl-card cl-qc" role="group" aria-label={questionLabel}>
      <div className="cl-qc-hd">
        <span className="q"><InlineMarkdown text={part.question} /></span>
        {parts.length > 1 && (
          <span className="pager">
            <button type="button" className="cl-ib" aria-label={t("cl_q_prev")} disabled={i === 0} onClick={() => setD({ ...d, page: i - 1 })}>
              <ChevronLeft size={14} />
            </button>
            <span className="tnum">{t("cl_q_page", { i: i + 1, n: parts.length })}</span>
            <button type="button" className="cl-ib" aria-label={t("cl_q_nextq")} disabled={last} onClick={() => setD({ ...d, page: i + 1 })}>
              <ChevronRight size={14} />
            </button>
          </span>
        )}
      </div>
      <div className="cl-qc-opts" role={multi ? "group" : "radiogroup"} aria-label={questionLabel}>
        {part.options.map((o) => {
          const on = d.picks[i]?.includes(o.label) ?? false;
          return (
            <button key={o.label} type="button" role={multi ? "checkbox" : "radio"} aria-checked={on} className={cx("opt", on && "on")} disabled={readonly}
              onClick={(e) => {
                if (questionSelectionBlocks(e.detail, window.getSelection(), e.currentTarget)) return;
                setD((cur) => qPick(cur, parts, i, o.label));
              }}>
              <span className={cx("mark", multi ? "check" : "radio")} aria-hidden>{multi && on && <Check size={11} />}</span>
              <span className="lb">
                <span className="nm"><InlineMarkdown text={o.label} /></span>
                {o.description && <span className="ds"><InlineMarkdown text={o.description} /></span>}
              </span>
            </button>
          );
        })}
        <button type="button" role={multi ? "checkbox" : "radio"} aria-checked={other !== null} className={cx("opt", other !== null && "on")} disabled={readonly}
          onClick={() => setD((cur) => qOther(cur, parts, i, cur.other[i] === null ? "" : multi ? null : cur.other[i]))}>
          <span className={cx("mark", multi ? "check" : "radio")} aria-hidden>{multi && other !== null && <Check size={11} />}</span>
          <span className="lb"><span className="nm">{t("cl_q_other")}</span></span>
        </button>
        {other !== null && (
          <textarea className="cl-qc-other" rows={1} autoFocus value={other} placeholder={t("cl_q_other_ph")} disabled={readonly}
            onChange={(e) => {
              const v = e.target.value;
              setD((cur) => qOther(cur, parts, i, v));
              e.target.style.height = "auto";
              e.target.style.height = `${Math.min(e.target.scrollHeight, 96)}px`;
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && !e.nativeEvent.isComposing) {
                e.preventDefault();
                if (!e.repeat) primary();
              }
            }} />
        )}
      </div>
      {readonly && <div className="cl-card-note">{t("cl_card_readonly")}</div>}
      <div className="cl-qc-ft">
        <button type="button" className="cl-btn" disabled={readonly} onMouseDown={(e) => e.preventDefault()} onClick={skip}>{t("cl_q_skip")}</button>
        <button type="button" className="cl-btn primary" disabled={readonly || !answered}
          title={!answered ? t("cl_q_need") : last && open >= 0 && open !== i ? t("cl_q_left", { n: open + 1 }) : undefined}
          onMouseDown={(e) => e.preventDefault()} onClick={primary}>
          {last ? t("cl_q_submit") : t("cl_q_next")}
          <Kbd k={CMD_ENTER} />
        </button>
      </div>
    </div>
  );
}
