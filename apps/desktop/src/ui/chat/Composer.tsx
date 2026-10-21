// The chat's composer: typing while the agent works (added to the running turn when the agent can
// take that, else queued for after it), the queued messages (edit, take back, send now), ↑ / ↓
// through what was sent, Stop, and how full the agent's context window is.
//
// Ported from Orca's native chat (stablyai/orca, MIT License, Copyright (c) 2026 Lovecast Inc.):
// the layout of NativeChatComposerField and NativeChatComposerActions (one rounded field, actions
// under the text, a round send button that turns into Stop), NativeChatQueuedMessageCard /
// NativeChatQueuedMessageEditor and NativeChatContextUsageRing. Rewritten for this app's commands.

import { ArrowUp, CornerDownRight, ListEnd, Pencil, Square, Trash2 } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { browseHistory, type InputMode } from "../../lib/chat";
import { t } from "../../lib/i18n";
import { cx } from "../util";

const RING = 16;
const STROKE = 2;
const R = (RING - STROKE) / 2;
const C = 2 * Math.PI * R;

function tokens(n: number): string {
  return n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1000 ? `${Math.round(n / 1000)}k` : String(n);
}

/** How full the agent's context window is; red past 90 %. */
export function ContextRing({ used, window: total }: { used: number; window: number }) {
  const p = Math.max(0, Math.min(1, used / total));
  const label = t("hc_context", { used: tokens(used), window: tokens(total), p: Math.round(p * 100) });
  return (
    <span className={cx("nt-ring", p >= 0.9 && "crit")} role="img" aria-label={label} title={label}>
      <svg width={RING} height={RING} viewBox={`0 0 ${RING} ${RING}`} aria-hidden>
        <circle cx={RING / 2} cy={RING / 2} r={R} className="track" strokeWidth={STROKE} fill="none" />
        <circle cx={RING / 2} cy={RING / 2} r={R} className="fill" strokeWidth={STROKE} fill="none" strokeDasharray={C}
          strokeDashoffset={C * (1 - p)} strokeLinecap="round" transform={`rotate(-90 ${RING / 2} ${RING / 2})`} />
      </svg>
      <span className="pct tnum">{Math.round(p * 100)}%</span>
    </span>
  );
}

function QueuedCard({ q, canSteer, onEdit, onRemove, onSteer, readonly }: {
  q: { id: string; text: string };
  canSteer: boolean;
  onEdit: (text: string) => void;
  onRemove: () => void;
  onSteer: () => void;
  readonly: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(q.text);
  const save = () => {
    const v = text.trim();
    if (v && v !== q.text) onEdit(v);
    setEditing(false);
  };
  if (editing) {
    return (
      <div className="nt-qd editing">
        <textarea
          autoFocus
          value={text}
          rows={Math.min(5, Math.max(1, text.split("\n").length))}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              setText(q.text);
              setEditing(false);
            }
            if (e.key !== "Enter" || e.shiftKey || e.nativeEvent.isComposing || e.keyCode === 229) return;
            e.preventDefault();
            save();
          }}
        />
        <div className="acts">
          <button type="button" className="btn sm" onClick={() => { setText(q.text); setEditing(false); }}>
            {t("hc_cancel")}
          </button>
          <button type="button" className="btn sm primary" disabled={!text.trim()} onClick={save}>
            {t("hc_save")}
          </button>
        </div>
      </div>
    );
  }
  return (
    <div className="nt-qd">
      <ListEnd size={14} className="ic" aria-hidden />
      <span className="tx" title={q.text}>
        {q.text}
      </span>
      {!readonly && (
        <span className="acts">
          {canSteer && (
            <button type="button" className="nt-ib" aria-label={t("hc_send_now")} title={t("hc_send_now")} onClick={onSteer}>
              <CornerDownRight size={13} />
            </button>
          )}
          <button type="button" className="nt-ib" aria-label={t("hc_edit_queued")} title={t("hc_edit_queued")} onClick={() => { setText(q.text); setEditing(true); }}>
            <Pencil size={13} />
          </button>
          <button type="button" className="nt-ib" aria-label={t("hc_dequeue")} title={t("hc_dequeue")} onClick={onRemove}>
            <Trash2 size={13} />
          </button>
        </span>
      )}
    </div>
  );
}

export interface ComposerProps {
  agent: string;
  host: string;
  running: boolean;
  /** The agent can take a message mid-turn (`caps.steer`). */
  canSteer: boolean;
  disabled: "ended" | "readonly" | null;
  context: { used: number; window: number } | null;
  queued: Array<{ id: string; text: string }>;
  history: string[];
  onSend: (text: string, mode: InputMode | undefined) => void;
  onStop: () => void;
  onDequeue: (id: string) => void;
  onEditQueued: (id: string, text: string) => void;
  onSteerQueued: (id: string, text: string) => void;
}

export function Composer(p: ComposerProps) {
  const [text, setText] = useState("");
  // Steering is the default where the agent supports it; the chip under the text switches it.
  const [steer, setSteer] = useState(true);
  const [at, setAt] = useState(p.history.length);
  const draft = useRef("");
  const area = useRef<HTMLTextAreaElement>(null);
  const mode: InputMode | undefined = p.running ? (p.canSteer && steer ? "steer" : "queue") : undefined;

  useEffect(() => setAt(p.history.length), [p.history.length]);
  // The field grows with its text up to a limit, then scrolls.
  useLayoutEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 168)}px`;
  }, [text]);

  const submit = (interrupt = false) => {
    const v = text.trim();
    if (!v || p.disabled) return;
    p.onSend(v, interrupt && p.running ? "interrupt" : mode);
    setText("");
    draft.current = "";
  };

  const placeholder =
    p.disabled === "ended" ? t("hc_ph_ended")
      : p.disabled === "readonly" ? t("hc_ph_readonly")
        : mode === "steer" ? t("hc_ph_steer")
          : mode === "queue" ? t("hc_ph_queue")
            : t("hc_ph", { agent: p.agent, host: p.host });

  return (
    <div className="nt-compose-wrap">
      {p.queued.length > 0 && (
        <div className="nt-queue" aria-label={t("hc_queued")}>
          {p.queued.map((q) => (
            <QueuedCard key={q.id} q={q} canSteer={p.canSteer && p.running} readonly={!!p.disabled}
              onEdit={(v) => p.onEditQueued(q.id, v)} onRemove={() => p.onDequeue(q.id)} onSteer={() => p.onSteerQueued(q.id, q.text)} />
          ))}
        </div>
      )}
      <div className={cx("nt-compose", p.disabled && "off")} onClick={() => area.current?.focus()}>
        <textarea
          ref={area}
          rows={1}
          value={text}
          disabled={!!p.disabled}
          placeholder={placeholder}
          aria-label={placeholder}
          onChange={(e) => {
            setText(e.target.value);
            setAt(p.history.length);
          }}
          onKeyDown={(e) => {
            const el = e.currentTarget;
            // ↑ / ↓ walk through sent messages when the caret is at the edge of the text.
            if ((e.key === "ArrowUp" || e.key === "ArrowDown") && !e.shiftKey && !e.altKey && !e.metaKey && !e.ctrlKey && !e.nativeEvent.isComposing) {
              const up = e.key === "ArrowUp";
              const atEdge = up ? el.selectionStart === 0 && el.selectionEnd === 0 : el.selectionStart === text.length;
              if (atEdge && (text === "" || at < p.history.length || !text.includes("\n"))) {
                if (at === p.history.length) draft.current = text;
                const next = browseHistory(p.history, at, up ? -1 : 1, draft.current);
                if (next) {
                  e.preventDefault();
                  setAt(next.at);
                  setText(next.text);
                  requestAnimationFrame(() => el.setSelectionRange(next.text.length, next.text.length));
                }
                return;
              }
            }
            if (e.key === "Escape" && p.running && !text) {
              e.preventDefault();
              p.onStop();
              return;
            }
            // An input method's Enter confirms the composition; it must not send.
            if (e.key !== "Enter" || e.shiftKey || e.nativeEvent.isComposing || e.keyCode === 229) return;
            e.preventDefault();
            submit(e.altKey);
          }}
        />
        <div className="bar">
          <div className="left">
            {p.running && !p.disabled && (
              p.canSteer ? (
                <button type="button" className={cx("chip", mode)} onClick={(e) => { e.stopPropagation(); setSteer((v) => !v); }}
                  title={mode === "steer" ? t("hc_mode_steer_d") : t("hc_mode_queue_d")} aria-pressed={mode === "steer"}>
                  {mode === "steer" ? <CornerDownRight size={12} aria-hidden /> : <ListEnd size={12} aria-hidden />}
                  {mode === "steer" ? t("hc_mode_steer") : t("hc_mode_queue")}
                </button>
              ) : (
                <span className="chip queue static" title={t("hc_mode_queue_d")}>
                  <ListEnd size={12} aria-hidden />
                  {t("hc_mode_queue")}
                </span>
              )
            )}
            {p.running && text.trim() && !p.disabled && <span className="hint">{t("hc_hint_alt")}</span>}
          </div>
          <div className="right">
            {p.context && <ContextRing used={p.context.used} window={p.context.window} />}
            {p.running && !p.disabled && (
              <button type="button" className="round stop" onClick={(e) => { e.stopPropagation(); p.onStop(); }} aria-label={t("hc_stop")} title={t("hc_stop_k")}>
                <Square size={12} fill="currentColor" />
              </button>
            )}
            {(!p.running || text.trim()) && (
              <button type="button" className="round send" onClick={(e) => { e.stopPropagation(); submit(); }} aria-label={t("send")} disabled={!text.trim() || !!p.disabled}>
                <ArrowUp size={15} />
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
