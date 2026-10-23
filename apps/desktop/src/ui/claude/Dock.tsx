// Around the Claude view's composer: the status line at the end of the transcript (CP-43 … CP-47: what
// the turn does now, how long it has run, its tokens; thinking says so in five steps by the second,
// a label waits 150 ms before it changes and then stays at least 650 ms), the task list docked above
// the composer (TR-42, TR-43), and the permission mode picker under it (CP-54, CP-55).

import { Circle, CircleCheck, ChevronDown, Hand, LoaderCircle, NotebookPen, ShieldOff, FilePen } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { Chat } from "../../lib/chat";
import {
  debounceStep,
  dwellStep,
  latestPlan,
  modeChange,
  MODES,
  planShown,
  statusOf,
  todoWindow,
  type Debounce,
  type Dwell,
  type Mode,
  type Status,
} from "../../lib/claude";
import { t, type Key } from "../../lib/i18n";
import { cx } from "../util";
import { countOf, dur, tokensLabel } from "./text";

// ---------------------------------------------------------------------------------------------
// The status line

/** The words for a status (without the timer and tokens). */
function phaseWords(s: Status, agent: string): string | null {
  switch (s.phase) {
    case "connecting":
      return t("cl_ph_connecting");
    case "sending":
      return t("cl_ph_sending");
    case "starting":
      return t("cl_ph_starting");
    case "stopping":
      return t("cl_ph_stopping");
    case "waiting":
      return t("cl_ph_waiting", { agent });
    case "tool":
      return t("cl_ph_tool");
    case "asking":
      return t("cl_ph_asking");
    case "thinking":
      return t(`cl_ph_think${s.tier ?? 0}` as Key);
    case "thought":
      // Reasoning that came in one piece has no duration worth saying.
      return (s.thoughtMs ?? 0) >= 1000 ? t("cl_ph_thought", { d: dur(s.thoughtMs ?? 0) }) : t("cl_ph_waiting", { agent });
    default:
      return null;
  }
}

/** A label through both flicker guards; wakes itself when a held value is due. */
function useSteady(value: string | null, epoch: number, holdEmpty: boolean): string | null {
  const deb = useRef<Debounce<string> | null>(null);
  const dw = useRef<Dwell<string> | null>(null);
  const [, bump] = useState(0);
  const now = Date.now();
  const a = debounceStep(deb.current, value, now, epoch, holdEmpty);
  deb.current = a.state;
  const b = dwellStep(dw.current, a.state.shown, now, epoch);
  dw.current = b.state;
  const wake = [a.wake, b.wake].filter((w): w is number => w !== null);
  useEffect(() => {
    if (!wake.length) return;
    const id = setTimeout(() => bump((n) => n + 1), Math.max(0, Math.min(...wake) - Date.now()));
    return () => clearTimeout(id);
  });
  return b.state.shown;
}

export function StatusLine({ chat, connected, sending, stopping, now, agent }: { chat: Chat; connected: boolean; sending: boolean; stopping: boolean; now: number; agent: string }) {
  const s = statusOf({ chat, connected, sending, stopping, now });
  const words = phaseWords(s, agent);
  const label = useSteady(words, s.epoch, s.on && s.tokens === 0 && s.phase !== "thinking");
  if (!s.on) return null;
  const parts: string[] = [];
  if (s.elapsedMs >= 2000) parts.push(dur(s.elapsedMs));
  if (s.elapsedMs >= 2000 && s.tokens > 0) parts.push(tokensLabel(s.tokens));
  const blink = s.phase !== "thought";
  return (
    <div className={cx("cl-status", s.phase)} role="status" aria-live="polite">
      <i className="cl-spark" aria-hidden />
      <span className="txt tnum">
        {parts.length > 0 && <span className="tm">{parts.join(" · ")}</span>}
        {label && (
          <span className={cx("ph", blink && "blink")}>
            {parts.length ? " · " : ""}
            {label}
          </span>
        )}
      </span>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// The task list above the composer

export function TodoDock({ chat }: { chat: Chat }) {
  const [open, setOpen] = useState(true);
  const plan = latestPlan(chat);
  if (!plan || !planShown(chat)) return null;
  const e = plan.entries;
  const w = todoWindow(e);
  const done = e.filter((x) => x.status === "completed").length;
  const working = chat.turns[chat.turns.length - 1]?.status === "running";
  return (
    <div className={cx("cl-todo", open && "open")}>
      <button type="button" className="hd" aria-expanded={open} aria-label={open ? t("cl_todo_hide") : t("cl_todo_show")} onClick={() => setOpen((v) => !v)}>
        <span className="ttl">{t("cl_todo_title")}</span>
        <span className="ct tnum">{t("cl_todo_count", { done, n: e.length })}</span>
        <ChevronDown size={13} className="chev" aria-hidden />
      </button>
      {open && (
        <div className="bd">
          {w.before > 0 && <div className="edge">{countOf("cl_todo_before", w.before)}</div>}
          <ul role="list">
            {e.slice(w.start, w.end).map((x, i) => (
              <li key={w.start + i} className={x.status}>
                {x.status === "completed" ? <CircleCheck size={14} aria-hidden />
                  : x.status === "in_progress" && working ? <LoaderCircle size={14} className="spin" aria-hidden />
                    : <Circle size={14} aria-hidden />}
                <span>{x.content}</span>
              </li>
            ))}
          </ul>
          {w.openAfter > 0 && <div className="edge">{countOf("cl_todo_after", w.openAfter)}</div>}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// The permission mode

const MODE_ICON: Record<Mode, typeof Hand> = { default: Hand, acceptEdits: FilePen, plan: NotebookPen, bypassPermissions: ShieldOff };

export function ModePicker({ mode, busy, agent, host, folder, disabled, onPick }: {
  mode: string | null;
  busy: boolean;
  agent: string;
  host: string;
  folder: string;
  disabled: boolean;
  onPick: (mode: Mode) => void;
}) {
  const [open, setOpen] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const box = useRef<HTMLSpanElement>(null);
  const cur = (MODES as readonly string[]).includes(mode ?? "") ? (mode as Mode) : null;
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => !box.current?.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", away);
    window.addEventListener("keydown", esc, true);
    return () => {
      document.removeEventListener("mousedown", away);
      window.removeEventListener("keydown", esc, true);
    };
  }, [open]);
  const pick = (m: Mode) => {
    setOpen(false);
    const c = modeChange(m, busy);
    if (c === "busy") setNote(t("cl_mode_busy"));
    else if (c === "confirm") setConfirm(true);
    else {
      setNote(null);
      onPick(m);
    }
  };
  const Ic = cur ? MODE_ICON[cur] : Hand;
  return (
    <span className="cl-mode" ref={box}>
      <button type="button" className={cx("cl-chip", cur === "bypassPermissions" && "warn")} disabled={disabled} aria-haspopup="menu" aria-expanded={open}
        aria-label={`${t("cl_mode_label")}: ${cur ? t(`cl_mode_${cur}` as Key) : "?"}`} onClick={(e) => { e.stopPropagation(); setOpen((v) => !v); }}>
        <Ic size={13} aria-hidden />
        <span>{cur ? t(`cl_mode_${cur}` as Key) : t("cl_mode_label")}</span>
        <ChevronDown size={12} aria-hidden />
      </button>
      {note && <span className="cl-chip-note" role="status">{note}</span>}
      {open && (
        <div className="cl-menu up" role="menu" aria-label={t("cl_mode_label")}>
          {MODES.map((m) => {
            const MI = MODE_ICON[m];
            return (
              <button key={m} type="button" role="menuitemradio" aria-checked={m === cur} className={cx(m === cur && "on", m === "bypassPermissions" && "warn")} onClick={() => pick(m)}>
                <MI size={14} aria-hidden />
                <span className="lb">
                  <span className="nm">{t(`cl_mode_${m}` as Key)}</span>
                  <span className="ds">{t(`cl_mode_${m}_d` as Key)}</span>
                </span>
              </button>
            );
          })}
        </div>
      )}
      {confirm && (
        <>
          <div className="smoke" style={{ position: "fixed" }} onClick={() => setConfirm(false)} />
          <div className="cdlg cl-confirm" role="alertdialog" aria-modal="true" aria-label={t("cl_mode_confirm_title")} style={{ position: "fixed" }}>
            <div className="b">
              <div className="ttl">{t("cl_mode_confirm_title")}</div>
              <div className="bd">{t("cl_mode_confirm_body", { agent, host, folder })}</div>
            </div>
            <div className="f">
              <button type="button" className="btn" autoFocus onClick={() => setConfirm(false)}>{t("cl_mode_cancel")}</button>
              <button type="button" className="btn danger" onClick={() => { setConfirm(false); onPick("bypassPermissions"); }}>{t("cl_mode_confirm_ok")}</button>
            </div>
          </div>
        </>
      )}
    </span>
  );
}
