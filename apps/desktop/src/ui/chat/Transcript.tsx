// The message list: turns as rows, the latest kept in view while the agent writes, a way back to the
// latest after scrolling up, and long conversations kept cheap.
//
// Ported from Orca's native chat (stablyai/orca, MIT License, Copyright (c) 2026 Lovecast Inc.):
// NativeChatMessageList's follow-the-end scrolling and NativeChatJumpToLatest, NativeChatWorkingStatus.
// Orca windows rows with measured heights; here only the newest turns are mounted (older ones on
// request), finished turns skip layout and paint while off screen (`content-visibility`), and a turn
// re-renders only when its own events arrive (each turn is memoized on its object).

import { ArrowDown } from "lucide-react";
import { memo, useLayoutEffect, useMemo, useRef, useState } from "react";
import { phase, stoppedTurn, turnRows, type Block, type Chat, type Turn } from "../../lib/chat";
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

export function Transcript({ chat, now, readonly, empty, workingLabel, follow, onAnswer, onContinue }: {
  chat: Chat;
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

  const jump = () => {
    const el = scroller.current;
    if (!el) return;
    stick.current = true;
    setAway(false);
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  };

  return (
    <div className="nt-transcript">
      <div
        className="nt-chat-body"
        ref={scroller}
        onScroll={(e) => {
          const el = e.currentTarget;
          const atEnd = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
          stick.current = atEnd;
          if (atEnd === away) setAway(!atEnd);
        }}
      >
        <div className="nt-chat-inner">
          {first > 0 && (
            <button type="button" className="nt-earlier" onClick={() => setShown((n) => n + WINDOW)}>
              {t("hc_earlier", { n: Math.min(first, WINDOW) })}
            </button>
          )}
          {chat.notices.map((n, i) => (
            <NoticeRow key={`n${i}`} level={n.level} code={n.code} text={n.text} />
          ))}
          {chat.turns.length === 0 && !running && <div className="nt-chat-empty">{empty}</div>}
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
