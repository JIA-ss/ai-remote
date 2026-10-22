// The chat's composer: typing while the agent works (the agent takes it the way it does natively:
// into the running turn for Claude Code and Codex, after the turn for the others), the queued messages (edit, take back, send now), ↑ / ↓
// through what was sent, Stop, and how full the agent's context window is.
//
// Ported from Orca's native chat (stablyai/orca, MIT License, Copyright (c) 2026 Lovecast Inc.):
// the layout of NativeChatComposerField and NativeChatComposerActions (one rounded field, actions
// under the text, a round send button that turns into Stop), NativeChatQueuedMessageCard /
// NativeChatQueuedMessageEditor and NativeChatContextUsageRing. Rewritten for this app's commands.

import { ArrowUp, FileText, ListEnd, Plus, Package, Pencil, Slash, Square, Trash2, X } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { browseHistory, sendMode, type Caps, type InputMode } from "../../lib/chat";
import {
  applyChoice,
  autocompleteAt,
  base64Bytes,
  fitWithin,
  imageProblem,
  IMAGE_TYPES,
  MAX_IMAGE_BYTES,
  MAX_IMAGES,
  type AttachmentPayload,
  type UsageReport,
  usageRows,
  withQuote,
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

/** How full the agent's context window is; red past 90 %. Opens what it is made of. */
export function ContextRing({ used, window: total, usage }: { used: number; window: number; usage?: UsageReport | null }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLSpanElement>(null);
  const p = Math.max(0, Math.min(1, used / total));
  const label = t("hc_context", { used: tokens(used), window: tokens(total), p: Math.round(p * 100) });
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);
  const rows = usageRows(usage ?? null, { used, window: total });
  return (
    <span className="nt-ring-box" ref={box}>
      <button type="button" className={cx("nt-ring", p >= 0.9 && "crit")} aria-label={label} title={open ? undefined : label} aria-expanded={open}
        onClick={(e) => { e.stopPropagation(); setOpen((v) => !v); }}>
        <svg width={RING} height={RING} viewBox={`0 0 ${RING} ${RING}`} aria-hidden>
          <circle cx={RING / 2} cy={RING / 2} r={R} className="track" strokeWidth={STROKE} fill="none" />
          <circle cx={RING / 2} cy={RING / 2} r={R} className="fill" strokeWidth={STROKE} fill="none" strokeDasharray={C}
            strokeDashoffset={C * (1 - p)} strokeLinecap="round" transform={`rotate(-90 ${RING / 2} ${RING / 2})`} />
        </svg>
        <span className="pct tnum">{Math.round(p * 100)}%</span>
      </button>
      {open && (
        <div className="nt-usage" role="dialog" aria-label={t("hc_usage_title")} onClick={(e) => e.stopPropagation()}>
          <div className="ttl">{t("hc_usage_title")}</div>
          <div className="meter"><i style={{ width: `${p * 100}%` }} /></div>
          <div className="sum tnum">{label}</div>
          <dl>
            {rows.map((r) => (
              <div key={r.key} className={cx("urow", r.sub && "sub")}>
                <dt>{t(`hc_usage_${r.key}`)}</dt>
                <dd className="tnum">{r.key === "cost" ? `$${r.value.toFixed(r.value < 1 ? 3 : 2)}` : r.value.toLocaleString()}</dd>
              </div>
            ))}
          </dl>
          <div className="note">{t("hc_usage_note")}</div>
        </div>
      )}
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

/** An image waiting in the composer: what goes to the host, and a picture for the chip. */
export interface ComposerImage {
  payload: AttachmentPayload;
  url: string;
}

function dataUrlOf(blob: Blob): Promise<string> {
  return new Promise((ok, fail) => {
    const r = new FileReader();
    r.onload = () => ok(String(r.result));
    r.onerror = () => fail(r.error ?? new Error("cannot read the image"));
    r.readAsDataURL(blob);
  });
}

/**
 * Reads a pasted or dropped image for sending: scaled down to a long edge of 2048 px (re-encoded
 * as JPEG, or PNG when it may be transparent) when it is larger or over the host's limit, as it is
 * otherwise. Errors are an i18n key.
 */
type ImageError = "hc_img_count" | "hc_img_type" | "hc_img_size" | "hc_img_unreadable";

async function readImage(file: File, have: number): Promise<ComposerImage | { error: ImageError }> {
  const problem = imageProblem(file.type || "application/octet-stream", 0, have);
  if (problem === "count") return { error: "hc_img_count" };
  if (problem === "type") return { error: "hc_img_type" };
  const name = file.name || `image.${file.type.split("/")[1] ?? "png"}`;
  let blob: Blob = file;
  const url = URL.createObjectURL(file);
  try {
    const img = new Image();
    img.src = url;
    await img.decode();
    const fit = fitWithin(img.naturalWidth, img.naturalHeight);
    if (fit.scaled || file.size > MAX_IMAGE_BYTES) {
      const canvas = document.createElement("canvas");
      canvas.width = fit.width;
      canvas.height = fit.height;
      canvas.getContext("2d")?.drawImage(img, 0, 0, fit.width, fit.height);
      const type = file.type === "image/png" || file.type === "image/gif" || file.type === "image/webp" ? "image/png" : "image/jpeg";
      blob = (await new Promise<Blob | null>((ok) => canvas.toBlob(ok, type, 0.88))) ?? file;
      if (blob.size > MAX_IMAGE_BYTES && type === "image/png") {
        blob = (await new Promise<Blob | null>((ok) => canvas.toBlob(ok, "image/jpeg", 0.85))) ?? blob;
      }
    }
  } catch {
    return { error: "hc_img_unreadable" };
  } finally {
    URL.revokeObjectURL(url);
  }
  const dataUrl = await dataUrlOf(blob);
  const data = dataUrl.slice(dataUrl.indexOf(",") + 1);
  const mime = blob.type || file.type;
  if (imageProblem(mime, base64Bytes(data), have) === "size") return { error: "hc_img_size" };
  return { payload: { name, mime, data }, url: dataUrl };
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
  onSend: (text: string, mode: InputMode | undefined, images: ComposerImage[]) => void;
  /** Images may go with a message; otherwise why not (shown on the disabled button). */
  images?: { ok: true } | { ok: false; why: string };
  onStop: () => void;
  onDequeue: (id: string) => void;
  onEditQueued: (id: string, text: string) => void;
  onSteerQueued: (id: string, text: string) => void;
  /** The agent's commands and skills, for the `/` menu (null: it has not said yet). */
  commands?: SlashCommand[] | null;
  /** The agent's last usage report, for the context ring's breakdown. */
  usage?: UsageReport | null;
  /** Text to add to the field (a quoted selection); `n` changes each time. */
  insert?: { text: string; n: number } | null;
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
  // A quoted selection goes below what is typed, with the caret after it.
  useEffect(() => {
    if (!p.insert) return;
    setText((v) => {
      const next = withQuote(v, p.insert!.text);
      requestAnimationFrame(() => {
        area.current?.focus();
        area.current?.setSelectionRange(next.length, next.length);
      });
      return next;
    });
  }, [p.insert?.n]);

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
    : ac.mode === "slash" ? (!p.commands?.length ? t("hc_ac_no_commands_yet") : items.length ? null : t("hc_ac_no_match"))
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

  // Images to send with the message.
  const [images, setImages] = useState<ComposerImage[]>([]);
  const [imageNote, setImageNote] = useState<string | null>(null);
  const picker = useRef<HTMLInputElement>(null);
  const canAttach = !p.disabled && p.images?.ok !== false;
  const attach = async (files: File[]) => {
    if (!canAttach) {
      setImageNote(p.images && !p.images.ok ? p.images.why : null);
      return;
    }
    let have = images.length;
    for (const f of files) {
      const r = await readImage(f, have);
      if ("error" in r) {
        setImageNote(t(r.error, { n: MAX_IMAGES, mb: (MAX_IMAGE_BYTES / 1048576).toFixed(2) }));
        continue;
      }
      have++;
      setImageNote(null);
      setImages((list) => (list.length < MAX_IMAGES ? [...list, r] : list));
    }
  };
  const imageFiles = (list: FileList | null | undefined) => [...(list ?? [])].filter((f) => f.type.startsWith("image/"));

  const submit = (interrupt = false) => {
    const v = text.trim();
    if ((!v && !images.length) || p.disabled) return;
    p.onSend(v, interrupt && p.running ? "interrupt" : mode, images);
    setText("");
    setImages([]);
    setImageNote(null);
    draft.current = "";
  };
  const ready = !!text.trim() || images.length > 0;

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
      <div
        className={cx("nt-compose", p.disabled && "off")}
        onClick={() => area.current?.focus()}
        onDragOver={(e) => {
          if (![...e.dataTransfer.items].some((i) => i.kind === "file")) return;
          // Dropped here, a file goes to the agent, not to the host's desktop (the stage's drop).
          e.preventDefault();
          e.stopPropagation();
          e.dataTransfer.dropEffect = canAttach ? "copy" : "none";
        }}
        onDrop={(e) => {
          const files = imageFiles(e.dataTransfer.files);
          if (!e.dataTransfer.files.length) return;
          e.preventDefault();
          e.stopPropagation();
          if (files.length) void attach(files);
          else setImageNote(t("hc_img_type"));
        }}
      >
        {images.length > 0 && (
          <div className="nt-atts">
            {images.map((im, i) => (
              <span key={i} className="att" title={im.payload.name}>
                <img src={im.url} alt={im.payload.name} />
                <button type="button" className="rm" aria-label={t("hc_img_remove", { name: im.payload.name })}
                  onClick={(e) => { e.stopPropagation(); setImages((list) => list.filter((_, j) => j !== i)); }}>
                  <X size={10} />
                </button>
              </span>
            ))}
          </div>
        )}
        <div className="line">
          {!p.disabled && (
            <>
              <button type="button" className={cx("nt-plus", !canAttach && "no")} aria-disabled={!canAttach}
                aria-label={canAttach ? t("hc_img_add") : p.images && !p.images.ok ? p.images.why : t("hc_img_add")}
                title={canAttach ? t("hc_img_add") : p.images && !p.images.ok ? p.images.why : undefined}
                onClick={(e) => { e.stopPropagation(); if (canAttach) picker.current?.click(); else void attach([]); }}>
                <Plus size={17} />
              </button>
              <input ref={picker} type="file" accept={IMAGE_TYPES.join(",")} multiple hidden
                onChange={(e) => { void attach(imageFiles(e.target.files)); e.target.value = ""; }} />
            </>
          )}
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
            onPaste={(e) => {
              const files = imageFiles(e.clipboardData?.files);
              if (!files.length) return;
              // Pasted text still goes in; the images become attachments.
              if (!e.clipboardData.getData("text/plain")) e.preventDefault();
              void attach(files);
            }}
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
          <div className="right">
            {p.running && !p.disabled && (
              <button type="button" className="round stop" onClick={(e) => { e.stopPropagation(); p.onStop(); }} aria-label={t("hc_stop")} title={t("hc_stop_k")}>
                <Square size={12} fill="currentColor" />
              </button>
            )}
            {(!p.running || ready) && (
              <button type="button" className="round send" onClick={(e) => { e.stopPropagation(); submit(); }} aria-label={t("send")} disabled={!ready || !!p.disabled}>
                <ArrowUp size={15} />
              </button>
            )}
          </div>
        </div>
      </div>
      {(imageNote || (p.running && ready && !p.disabled) || p.context) && (
        <div className="nt-compose-ft">
          <span className="hint-l">
            {imageNote && <span className="hint warn">{imageNote}</span>}
            {!imageNote && p.running && ready && !p.disabled && <span className="hint">{t("hc_hint_alt")}</span>}
          </span>
          {p.context && <ContextRing used={p.context.used} window={p.context.window} usage={p.usage} />}
        </div>
      )}
    </div>
  );
}
