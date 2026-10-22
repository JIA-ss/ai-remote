// The chat's composer: typing while the agent works (the agent takes it the way it does natively:
// into the running turn for Claude Code and Codex, after the turn for the others), the queued messages (edit, take back, send now), ↑ / ↓
// through what was sent, Stop, and how full the agent's context window is.
//
// Ported from Orca's native chat (stablyai/orca, MIT License, Copyright (c) 2026 Lovecast Inc.):
// the layout of NativeChatComposerField and NativeChatComposerActions (one rounded field, actions
// under the text, a round send button that turns into Stop), NativeChatQueuedMessageCard /
// NativeChatQueuedMessageEditor and NativeChatContextUsageRing. Rewritten for this app's commands.

import { ArrowUp, FileText, ListEnd, Package, Pencil, Slash, Square, Trash2 } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { browseHistory, sendMode, type Caps, type InputMode } from "../../lib/chat";
import {
  applyChoice,
  autocompleteAt,
  matchCommands,
  mentionText,
  moveActive,
  rankFiles,
  type Autocomplete,
  type SlashCommand,
} from "../../lib/chat-input";
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
              <ArrowUp size={13} />
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

/** One row of the `/` or `@` menu. */
type MenuItem = { key: string; kind: "command" | "skill" | "file"; label: string; hint?: string; detail?: string; insert: string };

/**
 * The `/` and `@` menu above the field (Orca's NativeChatPickerMenu / mention menu): commands, then
 * skills, or files; the highlighted row follows ↑ / ↓ and the mouse.
 */
function Menu({ ac, items, active, status, onChoose, onHover }: {
  ac: Autocomplete;
  items: MenuItem[];
  active: number;
  status: string | null;
  onChoose: (item: MenuItem) => void;
  onHover: (i: number) => void;
}) {
  const list = useRef<HTMLDivElement>(null);
  useEffect(() => {
    list.current?.querySelector<HTMLElement>(`[data-i="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [active, items]);
  const grouped = ac.mode === "slash" && items.some((i) => i.kind === "command") && items.some((i) => i.kind === "skill");
  return (
    <div className="nt-ac" role="listbox" aria-label={ac.mode === "slash" ? t("hc_ac_commands") : t("hc_ac_files")} ref={list} data-mode={ac.mode}>
      {items.map((item, i) => (
        <div key={item.key}>
          {grouped && (i === 0 || items[i - 1].kind !== item.kind) && (
            <div className="gh">{item.kind === "skill" ? t("hc_ac_skills") : t("hc_ac_commands")}</div>
          )}
          <button
            type="button"
            role="option"
            data-i={i}
            aria-selected={i === active}
            className={cx("it", i === active && "on")}
            onPointerDown={(e) => e.preventDefault()}
            onMouseEnter={() => onHover(i)}
            onClick={() => onChoose(item)}
          >
            {item.kind === "file" ? <FileText size={13} className="ic" /> : item.kind === "skill" ? <Package size={13} className="ic" /> : <Slash size={13} className="ic" />}
            <span className="nm"><bdi dir="ltr">{item.label}</bdi></span>
            {item.hint && <span className="hint">{item.hint}</span>}
            {item.detail && <span className="ds">{item.detail}</span>}
          </button>
        </div>
      ))}
      {status && <div className="st">{status}</div>}
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
  /** The agent's commands and skills, for the `/` menu (null: it has not said yet). */
  commands?: SlashCommand[] | null;
  /** Files in the session's folder matching a query, for the `@` menu. */
  listFiles?: (query: string) => Promise<{ files: string[]; truncated?: boolean }>;
}

export function Composer(p: ComposerProps) {
  const [text, setText] = useState("");
  const [at, setAt] = useState(p.history.length);
  const draft = useRef("");
  const area = useRef<HTMLTextAreaElement>(null);
  // As the agent itself does it: Claude Code and Codex take a message into the turn they are working
  // on; agents that cannot are sent it after the turn. There is no separate "add to this turn" action.
  const mode: InputMode | undefined = sendMode(p.running, { steer: p.canSteer } as Caps);

  useEffect(() => setAt(p.history.length), [p.history.length]);

  // The `/` and `@` menus: what the caret is in, unless Esc closed it for that word.
  const [caret, setCaret] = useState(0);
  const [closedAt, setClosedAt] = useState<number | null>(null);
  const [active, setActive] = useState(0);
  const [files, setFiles] = useState<{ query: string; files: string[]; truncated?: boolean; error?: string } | null>(null);
  const found = autocompleteAt(text, caret);
  const ac = found && found.start !== closedAt && !p.disabled ? found : null;
  const mentionQuery = ac?.mode === "mention" ? ac.query : null;
  useEffect(() => {
    if (mentionQuery === null || !p.listFiles) return;
    let live = true;
    // A short pause so typing a name does not ask the host per key.
    const timer = setTimeout(() => {
      p.listFiles!(mentionQuery).then(
        (r) => live && setFiles({ query: mentionQuery, files: r.files, truncated: r.truncated }),
        (e) => live && setFiles({ query: mentionQuery, files: [], error: e instanceof Error ? e.message : String(e) }),
      );
    }, files ? 120 : 0);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [mentionQuery]);
  const items: MenuItem[] = useMemo(() => {
    if (!ac) return [];
    if (ac.mode === "slash") {
      const list = matchCommands(p.commands ?? [], ac.query);
      // Commands first, then skills, each in the agent's order.
      return [...list.filter((c) => c.kind !== "skill"), ...list.filter((c) => c.kind === "skill")].map((c) => ({
        key: `${c.kind}:${c.name}`, kind: c.kind === "skill" ? "skill" : "command", label: c.insert, hint: c.hint, detail: c.description, insert: c.insert,
      }));
    }
    return rankFiles(files?.files ?? [], ac.query).map((f) => ({ key: `f:${f}`, kind: "file" as const, label: f, insert: mentionText(f) }));
  }, [ac?.mode, ac?.query, p.commands, files]);
  useEffect(() => setActive(0), [ac?.mode, ac?.query]);
  const menuStatus = !ac ? null
    : ac.mode === "slash" ? (p.commands == null ? t("hc_ac_no_commands_yet") : items.length ? null : t("hc_ac_no_match"))
      : !p.listFiles ? t("hc_ac_no_files")
        : files?.error ? t("hc_ac_files_error", { error: files.error })
          : files?.query !== ac.query && !items.length ? t("hc_ac_loading")
            : !items.length ? t("hc_ac_no_match")
              : files?.truncated ? t("hc_ac_more") : null;
  const choose = (item: MenuItem) => {
    if (!ac) return;
    const next = applyChoice(text, ac, item.insert);
    setText(next.text);
    setCaret(next.caret);
    requestAnimationFrame(() => {
      area.current?.focus();
      area.current?.setSelectionRange(next.caret, next.caret);
    });
  };
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
      {ac && (items.length > 0 || menuStatus) && (
        <Menu ac={ac} items={items} active={Math.min(active, Math.max(0, items.length - 1))} status={menuStatus} onChoose={choose} onHover={setActive} />
      )}
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
            setCaret(e.target.selectionStart ?? e.target.value.length);
            setClosedAt(null);
            setAt(p.history.length);
          }}
          onSelect={(e) => setCaret(e.currentTarget.selectionStart ?? 0)}
          onBlur={() => setClosedAt(ac?.start ?? null)}
          onFocus={() => setClosedAt(null)}
          onKeyDown={(e) => {
            const el = e.currentTarget;
            // The menu takes ↑ / ↓, Enter / Tab and Esc while it is open.
            if (ac && !e.nativeEvent.isComposing) {
              if ((e.key === "ArrowDown" || e.key === "ArrowUp") && items.length) {
                e.preventDefault();
                setActive((a) => moveActive(a, items.length, e.key === "ArrowUp" ? -1 : 1));
                return;
              }
              if ((e.key === "Enter" || e.key === "Tab") && !e.shiftKey && items.length) {
                e.preventDefault();
                choose(items[Math.min(active, items.length - 1)]);
                return;
              }
              if (e.key === "Escape") {
                e.preventDefault();
                setClosedAt(ac.start);
                return;
              }
            }
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
