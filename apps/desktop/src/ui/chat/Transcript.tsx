// The message list: turns as rows, the latest kept in view while the agent writes, a way back to the
// latest after scrolling up, and long conversations kept cheap.
//
// Ported from Orca's native chat (stablyai/orca, MIT License, Copyright (c) 2026 Lovecast Inc.):
// NativeChatMessageList's follow-the-end scrolling and NativeChatJumpToLatest, NativeChatWorkingStatus.
// Orca windows rows with measured heights; here only the newest turns are mounted (older ones on
// request), finished turns skip layout and paint while off screen (`content-visibility`), and a turn
// re-renders only when its own events arrive (each turn is memoized on its object).

import { ArrowDown, ChevronDown, ChevronUp, Quote, RotateCcw, X } from "lucide-react";
import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { findMatches, stepMatch } from "../../lib/chat-input";
import { phase, stoppedTurn, turnRows, type Block, type Chat, type Outgoing, type Turn } from "../../lib/chat";
import { t } from "../../lib/i18n";
import { cx } from "../util";
import { NoticeRow, RowView, TurnFoot, WorkingRow } from "./Rows";

/** Turns mounted at first; "show earlier" adds this many more each time. */
const WINDOW = 30;

function replyOf(turn: Turn): string {
  const texts = turn.blocks.filter((b): b is Extract<Block, { kind: "text" }> => b.kind === "text");
  return texts.length ? texts[texts.length - 1].text : turn.finalText ?? "";
}

const TurnView = memo(function TurnView({ turn, now, readonly, stopped, far, onAnswer, onContinue }: {
  turn: Turn;
  /** Not one of the last turns: skipped by layout and paint while off screen. */
  far: boolean;
  /** The clock, for running turns only (0 for finished ones, so they do not re-render each second). */
  now: number;
  readonly: boolean;
  stopped: boolean;
  onAnswer: (qid: string, text: string) => void;
  onContinue: () => void;
}) {
  const rows = useMemo(() => turnRows(turn), [turn]);
  return (
    <section className={cx("nt-turn", turn.status === "running" && "running", far && "far")} data-turn={turn.n}>
      {rows.map((r) => (
        <RowView key={r.key} row={r} now={now} readonly={readonly} onAnswer={onAnswer} />
      ))}
      <TurnFoot turn={turn} reply={replyOf(turn)} stopped={stopped} onContinue={onContinue} readonly={readonly} />
    </section>
  );
});

/** Why a message was not sent, in words. */
function failure(error: string | undefined): string {
  if (error === "timeout") return t("hc_out_timeout");
  if (error === "disconnected") return t("hc_out_disconnected");
  return error ? t("hc_out_refused", { why: error }) : t("hc_out_failed");
}

/**
 * A message on its way (Orca's NativeChatMessageList unsent message): "Sending…" until the host
 * has it, then shown like a sent message until the conversation does; "Not sent" with Retry when
 * it did not get there.
 */
function OutgoingRow({ o, onRetry, onDiscard }: { o: Outgoing; onRetry: (o: Outgoing) => void; onDiscard: (id: string) => void }) {
  return (
    <div className={cx("nt-user nt-out", o.state)} data-out={o.state}>
      <div className="bubble">
        {o.images && o.images.length > 0 && (
          <div className="imgs">
            {o.images.map((im, i) => <img key={i} src={im.url} alt={im.name} title={im.name} />)}
          </div>
        )}
        {o.text && <div className="txt">{o.text}</div>}
      </div>
      <div className="meta">
        {o.state === "sending" && <span className="st">{t("hc_out_sending")}</span>}
        {o.state === "failed" && (
          <>
            <span className="st bad" title={o.error}>{failure(o.error)}</span>
            <button type="button" className="nt-ib" onClick={() => onRetry(o)} aria-label={t("hc_out_retry")} title={t("hc_out_retry")}>
              <RotateCcw size={12} />
            </button>
            <button type="button" className="nt-ib" onClick={() => onDiscard(o.id)} aria-label={t("hc_out_discard")} title={t("hc_out_discard")}>
              <X size={12} />
            </button>
          </>
        )}
      </div>
    </div>
  );
}

/** The CSS Custom Highlight API where the web view has it (WebKit 17.2+, Chromium 105+). */
type Highlights = { set(name: string, h: unknown): void; delete(name: string): void };
function highlights(): { reg: Highlights; make: (ranges: Range[]) => unknown } | null {
  const css = (globalThis as { CSS?: { highlights?: Highlights } }).CSS;
  const H = (globalThis as { Highlight?: new (...r: Range[]) => unknown }).Highlight;
  return css?.highlights && H ? { reg: css.highlights, make: (r) => new H(...r) } : null;
}

/**
 * Find in the conversation (⌘F / Ctrl+F; Orca's NativeChatFindBar): the text shown now is searched
 * (folded parts and turns not mounted are not), matches are highlighted, Enter / Shift+Enter step
 * through them, Esc closes.
 */
function FindBar({ root, onShow, onClose }: { root: React.RefObject<HTMLElement | null>; onShow: (r: Range) => void; onClose: () => void }) {
  const [q, setQ] = useState("");
  const [cur, setCur] = useState(-1);
  const [ranges, setRanges] = useState<Range[]>([]);
  const [tick, setTick] = useState(0);
  const field = useRef<HTMLInputElement>(null);
  useEffect(() => field.current?.focus(), []);
  // New text (the agent writes on) searches again, at most a few times a second.
  useEffect(() => {
    const el = root.current;
    if (!el || typeof MutationObserver === "undefined") return;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const mo = new MutationObserver(() => {
      if (!timer) timer = setTimeout(() => { timer = null; setTick((n) => n + 1); }, 250);
    });
    mo.observe(el, { childList: true, subtree: true, characterData: true });
    return () => {
      mo.disconnect();
      if (timer) clearTimeout(timer);
    };
  }, [root]);
  useEffect(() => {
    const el = root.current;
    const found: Range[] = [];
    if (el && q) {
      const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
      for (let n = walker.nextNode(); n && found.length < 1000; n = walker.nextNode()) {
        for (const [a, b] of findMatches(n.nodeValue ?? "", q, 1000 - found.length)) {
          const r = document.createRange();
          r.setStart(n, a);
          r.setEnd(n, b);
          found.push(r);
        }
      }
    }
    setRanges(found);
    setCur((c) => (found.length ? Math.min(Math.max(c, 0), found.length - 1) : -1));
  }, [q, tick]);
  useEffect(() => {
    const h = highlights();
    if (h) {
      h.reg.set("nt-find", h.make(ranges));
      h.reg.set("nt-find-cur", h.make(cur >= 0 && ranges[cur] ? [ranges[cur]] : []));
    }
    return () => {
      h?.reg.delete("nt-find");
      h?.reg.delete("nt-find-cur");
    };
  }, [ranges, cur]);
  useEffect(() => {
    if (cur >= 0 && ranges[cur]) onShow(ranges[cur]);
  }, [cur, q]);
  const step = (dir: -1 | 1) => setCur((c) => stepMatch(c, ranges.length, dir));
  return (
    <div className="nt-findbar" role="search">
      <input
        ref={field}
        value={q}
        placeholder={t("hc_find_ph")}
        aria-label={t("hc_find_ph")}
        onChange={(e) => { setQ(e.target.value); setCur(0); }}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            step(e.shiftKey ? -1 : 1);
          } else if (e.key === "Escape") {
            e.preventDefault();
            onClose();
          }
        }}
      />
      <span className="ct tnum" aria-live="polite">{q ? (ranges.length ? `${cur + 1}/${ranges.length}` : t("hc_find_none")) : ""}</span>
      <button type="button" className="nt-ib" aria-label={t("hc_find_prev")} title={t("hc_find_prev")} disabled={!ranges.length} onClick={() => step(-1)}>
        <ChevronUp size={13} />
      </button>
      <button type="button" className="nt-ib" aria-label={t("hc_find_next")} title={t("hc_find_next")} disabled={!ranges.length} onClick={() => step(1)}>
        <ChevronDown size={13} />
      </button>
      <button type="button" className="nt-ib" aria-label={t("hc_find_close")} title={t("hc_find_close")} onClick={onClose}>
        <X size={13} />
      </button>
    </div>
  );
}

export function Transcript({ chat, now, readonly, empty, workingLabel, follow, outgoing = [], onRetry, onDiscard, onAnswer, onContinue, finding = false, onCloseFind, onQuote }: {
  chat: Chat;
  /** The find bar is open. */
  finding?: boolean;
  onCloseFind?: () => void;
  /** Quote a selection from the conversation in the composer. */
  onQuote?: (text: string) => void;
  outgoing?: Outgoing[];
  onRetry?: (o: Outgoing) => void;
  onDiscard?: (id: string) => void;
  /** Changes when this device sends something: the list jumps back to the end to follow it. */
  follow: number;
  now: number;
  readonly: boolean;
  empty: React.ReactNode;
  workingLabel: string;
  onAnswer: (qid: string, text: string) => void;
  onContinue: () => void;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const [away, setAway] = useState(false);
  const [shown, setShown] = useState(WINDOW);
  const running = chat.state === "running" || chat.state === "starting" || chat.state === "interrupting";
  const stopped = stoppedTurn(chat);
  const ph = phase(chat);
  const first = Math.max(0, chat.turns.length - shown);
  const visible = chat.turns.slice(first);

  useLayoutEffect(() => {
    if (!follow) return;
    stick.current = true;
    setAway(false);
  }, [follow]);
  // Follow the end while the reader is there; a reader who scrolled up stays where they are.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });
  // Content that grows without an event (a code block opening, fonts loading) keeps the end in view.
  useLayoutEffect(() => {
    const el = scroller.current;
    const inner = el?.firstElementChild;
    if (!el || !inner || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      if (stick.current) el.scrollTop = el.scrollHeight;
    });
    ro.observe(inner);
    return () => ro.disconnect();
  }, []);

  const inner = useRef<HTMLDivElement>(null);
  /** Scrolls a find match into view; the list stops following the end meanwhile. */
  const show = (r: Range) => {
    const el = scroller.current;
    if (!el) return;
    const box = r.getBoundingClientRect();
    const view = el.getBoundingClientRect();
    if (box.top < view.top + 40 || box.bottom > view.bottom - 40) {
      stick.current = false;
      setAway(true);
      el.scrollTop += box.top - view.top - el.clientHeight / 3;
    }
  };
  // A selection in the conversation offers to quote it (Orca's NativeChatSelectionQuote).
  const [quote, setQuote] = useState<{ text: string; top: number; left: number } | null>(null);
  const pickQuote = () => {
    if (!onQuote || readonly) return;
    const sel = window.getSelection();
    const el = scroller.current;
    if (!sel || sel.isCollapsed || !el || !sel.rangeCount || !el.contains(sel.anchorNode) || !el.contains(sel.focusNode)) {
      setQuote(null);
      return;
    }
    const text = sel.toString().trim();
    if (!text) return setQuote(null);
    const r = sel.getRangeAt(0).getBoundingClientRect();
    const box = el.parentElement!.getBoundingClientRect();
    setQuote({ text, top: Math.max(4, r.top - box.top - 34), left: Math.min(Math.max(4, r.left - box.left + r.width / 2 - 40), box.width - 90) });
  };
  useEffect(() => {
    if (!quote) return;
    const gone = () => {
      const sel = window.getSelection();
      if (!sel || sel.isCollapsed) setQuote(null);
    };
    document.addEventListener("selectionchange", gone);
    return () => document.removeEventListener("selectionchange", gone);
  }, [quote]);

  const jump = () => {
    const el = scroller.current;
    if (!el) return;
    stick.current = true;
    setAway(false);
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  };

  return (
    <div className="nt-transcript">
      {finding && <FindBar root={inner} onShow={show} onClose={() => onCloseFind?.()} />}
      {quote && (
        <button type="button" className="nt-quote" style={{ top: quote.top, left: quote.left }}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => { onQuote?.(quote.text); setQuote(null); window.getSelection()?.removeAllRanges(); }}>
          <Quote size={12} aria-hidden />
          <span>{t("hc_quote")}</span>
        </button>
      )}
      <div
        className="nt-chat-body"
        ref={scroller}
        onMouseUp={() => setTimeout(pickQuote, 0)}
        onKeyUp={(e) => e.shiftKey && pickQuote()}
        onScroll={(e) => {
          if (quote) setQuote(null);
          const el = e.currentTarget;
          const atEnd = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
          stick.current = atEnd;
          if (atEnd === away) setAway(!atEnd);
        }}
      >
        <div className="nt-chat-inner" ref={inner}>
          {first > 0 && (
            <button type="button" className="nt-earlier" onClick={() => setShown((n) => n + WINDOW)}>
              {t("hc_earlier", { n: Math.min(first, WINDOW) })}
            </button>
          )}
          {chat.notices.map((n, i) => (
            <NoticeRow key={`n${i}`} level={n.level} code={n.code} text={n.text} />
          ))}
          {chat.turns.length === 0 && !running && outgoing.length === 0 && <div className="nt-chat-empty">{empty}</div>}
          {visible.map((turn, i) => (
            <TurnView
              key={turn.n}
              turn={turn}
              now={turn.status === "running" ? now : 0}
              readonly={readonly}
              stopped={stopped === turn}
              onAnswer={onAnswer}
              onContinue={onContinue}
              far={i < visible.length - 2}
            />
          ))}
          {outgoing.map((o) => (
            <OutgoingRow key={o.id} o={o} onRetry={(x) => onRetry?.(x)} onDiscard={(id) => onDiscard?.(id)} />
          ))}
          {running && <WorkingRow label={workingLabel} since={ph.since} now={now} />}
        </div>
      </div>
      <button type="button" className="nt-jump" data-shown={away} inert={!away} aria-hidden={!away || undefined} onMouseDown={(e) => e.preventDefault()} onClick={jump}>
        <ArrowDown size={13} aria-hidden />
        <span>{t("hc_jump")}</span>
      </button>
    </div>
  );
}
