// The Claude view's transcript (TR-01 … TR-17): one centred column, the end followed while the reader is
// there (at the end within 2 px; a reply that starts within 8 px of it takes the follow back; scrolling
// or a key upwards lets go), a round "jump to the latest" button past 200 px and a fade past 8 px, the
// newest turns mounted (older ones on request), find (⌘F) and quoting a selection as in the chat panel.

import { ArrowDown, Quote } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { stoppedTurn, type Chat, type Outgoing } from "../../lib/chat";
import { FOLLOW, scrollKey } from "../../lib/claude";
import { t } from "../../lib/i18n";
import { cx } from "../util";
import { NoticeRow } from "../chat/Rows";
import { FindBar, OutgoingRow } from "../chat/Transcript";
import { ClaudeTurnView } from "./ClaudeTurn";

/** Turns mounted at first; "show earlier" adds this many more each time. */
const WINDOW = 30;

export function ClaudeTranscript({ chat, now, readonly, empty, follow, outgoing, store, onRetry, onDiscard, onContinue, finding, findFocus, onCloseFind, onQuote, status }: {
  chat: Chat;
  now: number;
  readonly: boolean;
  empty: ReactNode;
  /** Changes when this device sends something: back to the end to follow it. */
  follow: number;
  outgoing: Outgoing[];
  store: Map<string, boolean>;
  onRetry: (o: Outgoing) => void;
  onDiscard: (id: string) => void;
  onContinue: () => void;
  finding: boolean;
  findFocus: number;
  onCloseFind: () => void;
  onQuote?: (text: string) => void;
  /** The status line, at the end of the column. */
  status: ReactNode;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const inner = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const lastTop = useRef(0);
  const [gap, setGap] = useState(0);
  const [shown, setShown] = useState(WINDOW);
  const stopped = stoppedTurn(chat);
  const first = Math.max(0, chat.turns.length - shown);
  const visible = chat.turns.slice(first);
  const running = chat.state === "running" || chat.state === "starting" || chat.state === "interrupting";
  const last = chat.turns[chat.turns.length - 1];

  const distance = () => {
    const el = scroller.current;
    return el ? el.scrollHeight - el.scrollTop - el.clientHeight : 0;
  };
  const pin = () => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  };
  useLayoutEffect(() => {
    if (!follow) return;
    stick.current = true;
    setGap(0);
  }, [follow]);
  // A reply that starts close to the end takes the follow back (TR-14).
  useLayoutEffect(() => {
    if (last?.status === "running" && distance() <= FOLLOW.rejoin) stick.current = true;
  }, [last?.n, last?.status]);
  useLayoutEffect(() => {
    if (stick.current) pin();
  });
  // Content that grows without an event (a block opening, fonts) keeps the end in view.
  useLayoutEffect(() => {
    const el = scroller.current;
    const body = inner.current;
    if (!el || !body || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      if (stick.current) pin();
      else setGap(distance());
    });
    ro.observe(body);
    return () => ro.disconnect();
  }, []);

  /** Scrolls a find match into view; the list stops following meanwhile. */
  const show = (r: Range) => {
    const el = scroller.current;
    if (!el) return;
    const box = r.getBoundingClientRect();
    const view = el.getBoundingClientRect();
    if (box.top < view.top + 40 || box.bottom > view.bottom - 40) {
      stick.current = false;
      el.scrollTop += box.top - view.top - el.clientHeight / 3;
    }
  };
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
    setGap(0);
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  };

  return (
    <div className="cl-transcript">
      {finding && <FindBar root={inner} focus={findFocus} onShow={show} onClose={onCloseFind} />}
      {quote && (
        <button type="button" className="nt-quote" style={{ top: quote.top, left: quote.left }}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => { onQuote?.(quote.text); setQuote(null); window.getSelection()?.removeAllRanges(); }}>
          <Quote size={12} aria-hidden />
          <span>{t("hc_quote")}</span>
        </button>
      )}
      <div
        className="nt-chat-body cl-scroll"
        ref={scroller}
        role="feed"
        aria-busy={running}
        onMouseUp={() => setTimeout(pickQuote, 0)}
        onKeyUp={(e) => e.shiftKey && pickQuote()}
        onWheel={(e) => {
          if (e.deltaY < 0) stick.current = false;
        }}
        onKeyDown={(e) => {
          if ((e.target as HTMLElement).closest("textarea, input")) return;
          if (scrollKey(e.key, e.shiftKey) === "up") stick.current = false;
        }}
        onScroll={(e) => {
          if (quote) setQuote(null);
          const el = e.currentTarget;
          const d = el.scrollHeight - el.scrollTop - el.clientHeight;
          // Up past the end lets go (a drag of the scroll bar has no wheel); back at the end follows again.
          if (el.scrollTop < lastTop.current - 1 && d > FOLLOW.atEnd) stick.current = false;
          else if (d <= 1) stick.current = true;
          lastTop.current = el.scrollTop;
          setGap(stick.current ? 0 : d);
        }}
      >
        <div className="cl-col" ref={inner}>
          {first > 0 && (
            <button type="button" className="nt-earlier" onClick={() => setShown((n) => n + WINDOW)}>
              {t("hc_earlier", { n: Math.min(first, WINDOW) })}
            </button>
          )}
          {chat.notices.map((n, i) => (
            <NoticeRow key={`n${i}`} level={n.level} code={n.code} text={n.text} />
          ))}
          {chat.turns.length === 0 && !running && outgoing.length === 0 && <div className="cl-empty">{empty}</div>}
          {visible.map((turn, i) => (
            <ClaudeTurnView
              key={turn.n}
              turn={turn}
              requests={chat.requests}
              now={turn.status === "running" ? now : 0}
              readonly={readonly}
              stopped={stopped === turn}
              far={i < visible.length - 2}
              store={store}
              onContinue={onContinue}
            />
          ))}
          {outgoing.map((o) => (
            <OutgoingRow key={o.id} o={o} onRetry={onRetry} onDiscard={onDiscard} />
          ))}
          {status}
        </div>
      </div>
      <div className={cx("cl-fade", gap > FOLLOW.fade && "on")} aria-hidden />
      <button type="button" className="cl-jump" data-shown={gap > FOLLOW.jump} data-busy={running || undefined} inert={gap <= FOLLOW.jump}
        aria-label={t("cl_jump")} title={t("cl_jump")} onMouseDown={(e) => e.preventDefault()} onClick={jump}>
        <ArrowDown size={15} aria-hidden />
      </button>
    </div>
  );
}
