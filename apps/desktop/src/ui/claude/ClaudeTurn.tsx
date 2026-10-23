// One turn of a Claude conversation, laid out the way Claude's own app does it (TR-08, TR-28 … TR-40,
// TC-01 … TC-17): the user's message as a bubble at the end, prose without a bubble, tool calls and
// reasoning in a row folded into one sentence ("Read 2 files, ran 3 commands") that stays open while it
// runs or waits for approval, a sub-agent as its own row, answered questions as a receipt, the files the
// turn changed, and a foot with copy and the time it took. Rows are lib/claude.ts's.

import { ChevronRight, CircleAlert, FilePen, Play } from "lucide-react";
import { memo, useEffect, useMemo, useRef, useState } from "react";
import { imagesOf } from "../../lib/chat-input";
import { baseName, toolOutput, type Request, type ThoughtBlock, type ToolItem, type Turn } from "../../lib/chat";
import {
  claudeRows,
  dwellStep,
  folderHints,
  groupSentence,
  groupStat,
  NO_PREFERENCE,
  OTHER_EMPTY,
  paceNext,
  paceStart,
  questionParts,
  requestsByCall,
  showChanges,
  singleCall,
  toolName,
  toolState,
  turnChanges,
  type ClRow,
  type Dwell,
  type ToolState,
} from "../../lib/claude";
import { t } from "../../lib/i18n";
import { cx } from "../util";
import { CopyButton, StreamingMarkdown } from "../chat/Markdown";
import { NoticeRow } from "../chat/Rows";
import { ToolBody } from "./ToolBody";
import { countOf, dur, SentenceView, ToolLabel } from "./text";

// ---------------------------------------------------------------------------------------------
// What is open, per session (TR-33): kept while the view mounts rows again, for the last 50 sessions.

const OPEN = new Map<string, Map<string, boolean>>();

export function openStore(session: string): Map<string, boolean> {
  let m = OPEN.get(session);
  if (m) OPEN.delete(session);
  else m = new Map();
  OPEN.set(session, m);
  while (OPEN.size > 50) OPEN.delete(OPEN.keys().next().value as string);
  return m;
}

/** Open or not: what the person chose for `key`, else `dflt` (which may change, e.g. a group that ends). */
function useOpen(store: Map<string, boolean>, key: string, dflt: boolean): [boolean, () => void] {
  const [, bump] = useState(0);
  const chosen = store.get(key);
  const open = chosen ?? dflt;
  return [open, () => { store.set(key, !open); bump((n) => n + 1); }];
}

/** A label that changes at most every 650 ms (TR-31, TC-12); `epoch` starts it over. */
function useDwell<T>(value: T | null, epoch: number): T | null {
  const st = useRef<Dwell<T> | null>(null);
  const [, bump] = useState(0);
  const r = dwellStep(st.current, value, Date.now(), epoch);
  st.current = r.state;
  useEffect(() => {
    if (r.wake === null) return;
    const id = setTimeout(() => bump((n) => n + 1), Math.max(0, r.wake - Date.now()));
    return () => clearTimeout(id);
  });
  return r.state.shown;
}

// ---------------------------------------------------------------------------------------------
// Rows

const FOLD_CHARS = 1200;

function UserBubble({ row }: { row: Extract<ClRow, { kind: "user" }> }) {
  const long = row.text.length > FOLD_CHARS || row.text.split("\n").length > 14;
  const [open, setOpen] = useState(false);
  const mine = imagesOf(row.inputId);
  return (
    <div className="nt-user cl-user" data-steer={row.steer ? "1" : undefined}>
      <div className={cx("bubble", long && !open && "folded")}>
        {row.attachments && row.attachments.length > 0 && (
          <div className="imgs">
            {row.attachments.map((a, i) => (mine[i] ? <img key={i} src={mine[i].url} alt={a.name} title={a.name} /> : <span key={i} className="att" title={a.mime}>{a.name}</span>))}
          </div>
        )}
        {row.text && <div className="txt">{row.text}</div>}
      </div>
      {long && (
        <button type="button" className="cl-fold" onClick={() => setOpen((v) => !v)} aria-expanded={open}>
          {open ? t("hc_show_less") : t("hc_show_more")}
        </button>
      )}
      <div className="cl-acts">
        <CopyButton text={row.text} />
      </div>
    </div>
  );
}

/** A streamed reply shown at a steady pace (TR-24): about 22 characters a step, ~550 ms behind. A page
 *  that is not visible shows everything at once (it has no frames to pace with). */
function usePaced(text: string, streaming: boolean): string {
  const shown = useRef(streaming ? paceStart(text, false) : text.length);
  const latest = useRef(text);
  const lastLen = useRef(text.length);
  const arrived = useRef(0);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [, render] = useState(0);
  latest.current = text;
  const caught = !streaming || document.hidden;
  useEffect(() => {
    arrived.current += Math.max(0, text.length - lastLen.current);
    lastLen.current = text.length;
    if (caught) {
      if (timer.current) clearTimeout(timer.current);
      timer.current = null;
      shown.current = text.length;
      return;
    }
    // One step at a time; text arriving meanwhile does not restart the wait (it comes faster than a step).
    const next = () => {
      if (timer.current) return;
      const step = paceNext(shown.current, latest.current, arrived.current);
      arrived.current = 0;
      if (step.delay === null) {
        if (step.next !== shown.current) {
          shown.current = step.next;
          render((n) => n + 1);
        }
        return;
      }
      timer.current = setTimeout(() => {
        timer.current = null;
        shown.current = step.next;
        render((n) => n + 1);
        next();
      }, step.delay);
    };
    next();
  }, [text, caught]);
  useEffect(() => () => {
    if (timer.current) clearTimeout(timer.current);
  }, []);
  return caught ? text : text.slice(0, Math.min(shown.current, text.length));
}

function Prose({ row }: { row: Extract<ClRow, { kind: "text" }> }) {
  const paced = usePaced(row.text, row.streaming);
  // A fence that has only opened would flash as an empty code block before its first line comes.
  const text = row.streaming ? paced.replace(/(^|\n)[ \t>]*(?:`{1,}|~{1,})[\w+-]*[ \t]*$/, "$1") : paced;
  if (!text) return null;
  return <StreamingMarkdown text={text} streaming={row.streaming} className={cx("cl-md", row.streaming && "streaming")} />;
}

/** "+3 −1" of an edit. */
function Stat({ s }: { s: { add: number; del: number } | null }) {
  if (!s || s.add + s.del === 0) return null;
  return (
    <span className="cl-stat tnum">
      <span className="add">+{s.add}</span>
      <span className="del">−{s.del}</span>
    </span>
  );
}

function StateWord({ st }: { st: ToolState }) {
  if (st === "failed") return <span className="cl-stw err">{t("cl_st_failed")}</span>;
  if (st === "denied") return <span className="cl-stw">{t("cl_st_denied")}</span>;
  if (st === "stopped") return <span className="cl-stw">{t("cl_st_stopped")}</span>;
  if (st === "blocked") return <span className="cl-stw warn">{t("cl_st_blocked")}</span>;
  return null;
}

/** Whether an opened row has anything to show. */
function hasBody(x: ToolItem): boolean {
  if (x.content.length || x.input !== undefined) return true;
  return toolName(x) === "Bash" || x.tool === "execute";
}

/** One tool call: a line that opens to its command and output, a file, a diff, … (TC-14, TC-15). */
const ToolRow = memo(function ToolRow({ x, st, now, store, nested }: { x: ToolItem; st: ToolState; now: number; store: Map<string, boolean>; nested?: boolean }) {
  const [open, toggle] = useOpen(store, `t:${x.callId}`, false);
  const shown = useDwell<ToolState>(st, 0) ?? st;
  const body = hasBody(x);
  const elapsed = shown === "busy" ? now - x.startedTs : 0;
  const stat = shown !== "busy" && x.content.some((c) => c.type === "diff") ? groupStat([x]) : null;
  return (
    <div className={cx("cl-tool", nested && "nested", open && "open")} data-state={shown}>
      <button type="button" className="cl-tool-hd" aria-expanded={body ? open : undefined} disabled={!body} onClick={toggle}>
        <ChevronRight size={12} className="chev" aria-hidden />
        <span className={cx("cl-line", shown === "busy" && "shimmer")}>
          <ToolLabel x={x} st={shown} />
        </span>
        {shown === "busy" && <span className="sr-only">{t("cl_st_running")}</span>}
        <StateWord st={shown} />
        <Stat s={stat} />
        {elapsed >= 2000 && <span className="cl-tm tnum">{dur(elapsed)}</span>}
      </button>
      {open && body && <ToolBody x={x} st={shown} now={now} />}
    </div>
  );
});

/** Reasoning: "Thinking" while it comes, "Thought for 5s" after; opens to the text (TR-36). */
const ThoughtRow = memo(function ThoughtRow({ b, live, store }: { b: ThoughtBlock; live: boolean; store: Map<string, boolean> }) {
  const [open, toggle] = useOpen(store, `r:${b.id}`, false);
  const ms = Math.max(0, b.endedTs - b.startedTs);
  return (
    <div className={cx("cl-tool cl-thought nested", open && "open")} data-state={live ? "busy" : "done"}>
      <button type="button" className="cl-tool-hd" aria-expanded={open} onClick={toggle}>
        <ChevronRight size={12} className="chev" aria-hidden />
        <span className={cx("cl-line", live && "shimmer")}>
          {live ? t("cl_g_thinking") : ms >= 1000 ? t("cl_th_for", { d: dur(ms) }) : t("cl_g_thought")}
        </span>
      </button>
      {open && <div className="cl-thought-bd">{b.text}</div>}
    </div>
  );
});

/** Tool calls and reasoning in a row: one sentence when done, open while running or waiting (TR-28 … TR-33). */
const GroupRow = memo(function GroupRow({ row, states, now, store }: { row: Extract<ClRow, { kind: "group" }>; states: Map<string, ToolState>; now: number; store: Map<string, boolean> }) {
  const blocked = row.tools.some((x) => states.get(x.callId) === "blocked");
  const busyTool = [...row.tools].reverse().find((x) => states.get(x.callId) === "blocked") ?? [...row.tools].reverse().find((x) => states.get(x.callId) === "busy") ?? null;
  const [open, toggle] = useOpen(store, row.key, row.live || blocked);
  // While it runs the header says what runs now: the call, or reasoning that is still coming in; between
  // calls it says what was done so far.
  const thinking = row.live && row.members[row.members.length - 1]?.kind === "thought";
  const current = useDwell<string>(row.live || blocked ? busyTool?.callId ?? (thinking ? "think" : null) : null, 0);
  const shownTool = current ? row.tools.find((x) => x.callId === current) ?? null : null;
  const stat = groupStat(row.tools);
  const thoughts = row.members.filter((m): m is ThoughtBlock => m.kind === "thought");
  let label: React.ReactNode;
  if (current && shownTool) label = <span className="cl-line shimmer"><ToolLabel x={shownTool} st={states.get(shownTool.callId) ?? "busy"} /></span>;
  else if (current) label = <span className="cl-line shimmer">{t("cl_g_thinking")}</span>;
  else if (!row.tools.length) {
    const ms = thoughts.reduce((a, b) => a + Math.max(0, b.endedTs - b.startedTs), 0);
    label = <span className="cl-line">{ms >= 1000 ? t("cl_th_for", { d: dur(ms) }) : t("cl_g_thought")}</span>;
  } else label = <span className="cl-line"><SentenceOf tools={row.tools} states={states} /></span>;
  return (
    <div className={cx("cl-group", open && "open", (row.live || blocked) && "is-live")}>
      <button type="button" className="cl-group-hd" aria-expanded={open} onClick={toggle}>
        {label}
        {blocked && <span className="cl-stw warn">{t("cl_st_blocked")}</span>}
        {!row.live && <Stat s={stat} />}
        <ChevronRight size={13} className="chev" aria-hidden />
      </button>
      {open && (
        <div className="cl-group-bd">
          {row.members.map((m) =>
            m.kind === "tool" ? (
              <ToolRow key={m.callId} x={m} st={states.get(m.callId) ?? "done"} now={now} store={store} nested />
            ) : (
              <ThoughtRow key={m.id} b={m} live={row.live && m === row.members[row.members.length - 1]} store={store} />
            ),
          )}
        </div>
      )}
    </div>
  );
});

function SentenceOf({ tools, states }: { tools: ToolItem[]; states: Map<string, ToolState> }) {
  const s = useMemo(() => groupSentence(tools, states), [tools, states]);
  return <SentenceView s={s} />;
}

/** A sub-agent: its line (with its latest step while it runs), opening to its own steps and its answer (TC-08). */
const AgentRow = memo(function AgentRow({ row, states, now, store }: { row: Extract<ClRow, { kind: "agent" }>; states: Map<string, ToolState>; now: number; store: Map<string, boolean> }) {
  const st = states.get(row.tool.callId) ?? toolState(row.tool);
  const [open, toggle] = useOpen(store, row.key, false);
  const last = row.children[row.children.length - 1];
  const out = toolOutput(row.tool);
  return (
    <div className={cx("cl-agent", open && "open")} data-state={st}>
      <button type="button" className="cl-tool-hd" aria-expanded={open} onClick={toggle}>
        <ChevronRight size={12} className="chev" aria-hidden />
        <span className={cx("cl-line", st === "busy" && "shimmer")}>
          <ToolLabel x={row.tool} st={st} />
        </span>
        <StateWord st={st} />
        {row.children.length > 0 && (
          <span className="cl-steps tnum">
            {st === "busy" && last ? t("cl_agent_last", { tool: (toolName(last) || last.title).slice(0, 24), n: row.children.length }) : countOf("cl_g_tool", row.children.length)}
          </span>
        )}
      </button>
      {open && (
        <div className="cl-agent-bd">
          {row.children.map((c) => (
            <ToolRow key={c.callId} x={c} st={states.get(c.callId) ?? toolState(c)} now={now} store={store} nested />
          ))}
          {out && st !== "busy" && <StreamingMarkdown text={out} streaming={false} className="cl-md cl-agent-out" />}
        </div>
      )}
    </div>
  );
});

/** A question that was answered (or fell away): each part and what was said (TC-50). */
function QuestionReceipt({ row }: { row: Extract<ClRow, { kind: "question" }> }) {
  const b = row.block;
  if (!b.answered && !b.outcome) return null;
  const parts = questionParts(b);
  const answerOf = (q: string, i: number): string | null => {
    const a = b.answers?.[q] ?? (i === 0 ? b.answer : undefined);
    if (a === undefined || a === "") return null;
    if (a === NO_PREFERENCE) return t("cl_q_no_pref");
    return a.replace(OTHER_EMPTY, t("cl_q_other_empty"));
  };
  return (
    <div className="cl-receipt">
      {parts.map((p, i) => {
        const a = answerOf(p.question, i);
        return (
          <div key={i} className="part">
            <div className="q">{p.question}</div>
            {/* The host keeps no answers: a conversation opened again knows only that it was answered. */}
            <div className={cx("a", !a && "none")}>{a ?? (b.answered || b.outcome === "answered" ? t("cl_q_answered") : t("cl_q_cancelled"))}</div>
          </div>
        );
      })}
    </div>
  );
}

/** The files a turn changed (TR-40): a title with the counts, the first three, the rest behind "N more". */
function ChangesCard({ turn, store }: { turn: Turn; store: Map<string, boolean> }) {
  const files = useMemo(() => turnChanges(turn), [turn]);
  const [all, toggle] = useOpen(store, `ch:${turn.n}`, false);
  const hints = useMemo(() => folderHints(files.map((f) => f.path)), [files]);
  if (!files.length) return null;
  const add = files.reduce((a, f) => a + f.add, 0);
  const del = files.reduce((a, f) => a + f.del, 0);
  const shown = all ? files : files.slice(0, 3);
  return (
    <div className="cl-changes">
      <div className="hd">
        <FilePen size={14} aria-hidden />
        <span className="ttl">{countOf("cl_ch_title", files.length)}</span>
        <Stat s={{ add, del }} />
      </div>
      {shown.map((f) => (
        <div key={f.path} className="file" title={f.path}>
          <span className="fn">{baseName(f.path)}</span>
          {hints.get(f.path) && <span className="hint">{hints.get(f.path)}</span>}
          {f.created && <span className="new">{t("cl_ch_new")}</span>}
          <Stat s={f} />
        </div>
      ))}
      {files.length > 3 && (
        <button type="button" className="more" onClick={toggle}>
          {all ? t("cl_ch_less") : t("cl_ch_more", { n: files.length - 3 })}
        </button>
      )}
    </div>
  );
}

function replyOf(turn: Turn): string {
  const texts = turn.blocks.filter((b): b is Extract<Turn["blocks"][number], { kind: "text" }> => b.kind === "text");
  return texts.length ? texts[texts.length - 1].text : turn.finalText ?? "";
}

/** Under a finished turn: copy the reply and how long it took; a stopped last turn offers to continue. */
function TurnFoot({ turn, stopped, onContinue, readonly }: { turn: Turn; stopped: boolean; onContinue: () => void; readonly: boolean }) {
  if (turn.status === "running") return null;
  const reply = replyOf(turn);
  return (
    <>
      {turn.status === "failed" && (
        <div className="cl-fail" role="status">
          <CircleAlert size={14} aria-hidden />
          <span>
            <b>{t("cl_failed")}</b>
            {turn.reason ? `: ${turn.reason}` : ""}
          </span>
        </div>
      )}
      {turn.status === "interrupted" && stopped && !readonly ? (
        <div className="cl-stopped">
          <span>{turn.reason ? t("cl_stopped_why", { why: turn.reason }) : t("cl_stopped")}</span>
          <button type="button" className="cl-btn" onClick={onContinue}>
            <Play size={12} aria-hidden />
            {t("cl_continue")}
          </button>
        </div>
      ) : (
        <div className={cx("cl-foot", turn.status)}>
          {turn.status === "interrupted" && <span className="st">{t("cl_st_stopped")}</span>}
          {reply && <CopyButton text={reply} label={t("cl_copy_reply")} />}
          {turn.durationMs != null && <span className="meta tnum">{dur(turn.durationMs)}</span>}
        </div>
      )}
    </>
  );
}

// ---------------------------------------------------------------------------------------------
// The turn

export const ClaudeTurnView = memo(function ClaudeTurnView({ turn, requests, now, readonly, stopped, far, store, onContinue }: {
  turn: Turn;
  /** The conversation's permissions and plans (the turn picks its own). */
  requests: Request[];
  /** The clock, for running turns only. */
  now: number;
  readonly: boolean;
  stopped: boolean;
  far: boolean;
  store: Map<string, boolean>;
  onContinue: () => void;
}) {
  const rows = useMemo(() => claudeRows(turn), [turn]);
  const states = useMemo(() => {
    const by = requestsByCall(turn, requests);
    const m = new Map<string, ToolState>();
    for (const b of turn.blocks) if (b.kind === "tool") m.set(b.callId, toolState(b, by.get(b.callId)));
    return m;
  }, [turn, requests]);
  return (
    <section className={cx("cl-turn", turn.status === "running" && "running", far && "far")} data-turn={turn.n}>
      {rows.map((r) => {
        switch (r.kind) {
          case "user":
            return <UserBubble key={r.key} row={r} />;
          case "text":
            return <Prose key={r.key} row={r} />;
          case "group": {
            // One call alone is its own row, running or not (a group around it would say the same twice).
            const one = singleCall(r) ?? (r.members.length === 1 && r.tools.length === 1 ? r.tools[0] : null);
            return one ? <ToolRow key={r.key} x={one} st={states.get(one.callId) ?? "done"} now={now} store={store} />
              : <GroupRow key={r.key} row={r} states={states} now={now} store={store} />;
          }
          case "agent":
            return <AgentRow key={r.key} row={r} states={states} now={now} store={store} />;
          case "question":
            return <QuestionReceipt key={r.key} row={r} />;
          case "notice":
            return <NoticeRow key={r.key} level={r.level} code={r.code} text={r.text} />;
        }
      })}
      {showChanges(turn, rows) && <ChangesCard turn={turn} store={store} />}
      <TurnFoot turn={turn} stopped={stopped} onContinue={onContinue} readonly={readonly} />
    </section>
  );
});
