// The rows of a chat turn: the user's message, a run of tool calls folded into one sentence, a tool
// line with its command and output, an edit as a diff card, reasoning, a plan, a sub-agent, a
// question, a notice, and how the turn ended.
//
// Ported from Orca's native chat (stablyai/orca, MIT License, Copyright (c) 2026 Lovecast Inc.):
// the layout, spacing, folding and the wording of NativeChatToolRun ("Ran 2 commands, read 1
// file", present tense while live), NativeChatToolLine, NativeChatDiffCard, NativeChatReasoningRow,
// NativeChatTaskList, NativeChatSubagentRun, NativeChatQuestionCard, NativeChatUserMessageRow and
// NativeChatInterruptedContinue. Rewritten against this app's events (lib/chat.ts) and CSS.

import {
  Bot,
  Brain,
  Check,
  ChevronRight,
  Circle,
  CircleAlert,
  CircleCheck,
  CircleDot,
  FileMinus2,
  FilePen,
  FilePlus2,
  FileText,
  Globe,
  Info,
  ListTodo,
  MessageCircleQuestion,
  Play,
  Search,
  SquareTerminal,
  TriangleAlert,
  Wrench,
  X,
} from "lucide-react";
import { memo, useEffect, useMemo, useState, type ReactNode } from "react";
import {
  baseName,
  diffRows,
  diffStat,
  isEdit,
  patchText,
  runDiffStat,
  runOutcome,
  runSummary,
  tailLines,
  toolCommand,
  toolLabel,
  toolOutput,
  type DiffRow,
  type PlanEntry,
  type QuestionBlock,
  type Row,
  type RunCategory,
  type ThoughtBlock,
  type ToolContent,
  type ToolItem,
  type ToolKind,
  type Turn,
} from "../../lib/chat";
import { t, type Key } from "../../lib/i18n";
import { inlineText, parseInline } from "../../lib/markdown";
import { cx } from "../util";
import { CodeBlock, CopyButton, Highlighted, Markdown } from "./Markdown";

/** "1.2 s", "38 s", "2:05". */
export function secs(ms: number) {
  return ms < 1000 ? `${Math.max(0, Math.round(ms))} ms` : ms < 60_000 ? `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)} s` : `${Math.floor(ms / 60_000)}:${String(Math.round((ms % 60_000) / 1000) % 60).padStart(2, "0")}`;
}

/** The string for a count: `<key>_one` for 1, `<key>_many` otherwise (Chinese uses the same text). */
export function plural(key: string, n: number, vars: Record<string, string | number> = {}): string {
  return t(`${key}_${n === 1 ? "one" : "many"}` as Key, { n, ...vars });
}

function Chevron({ open }: { open: boolean }) {
  return <ChevronRight size={13} className={cx("nt-chev", open && "open")} aria-hidden />;
}

function Spinner() {
  return <i className="nt-cspin" aria-hidden />;
}

const KIND_ICON: Record<ToolKind, typeof Wrench> = {
  execute: SquareTerminal,
  read: FileText,
  edit: FilePen,
  delete: FileMinus2,
  move: FilePen,
  search: Search,
  fetch: Globe,
  think: Brain,
  other: Wrench,
};

const CAT_ICON: Record<RunCategory, typeof Wrench> = {
  command: SquareTerminal,
  read: FileText,
  search: Search,
  web: Globe,
  change: FilePen,
  tool: Wrench,
};

// ---------------------------------------------------------------------------------------------
// The user's message

/** Sent messages longer than this many lines fold behind "Show more". */
const FOLD_LINES = 12;

export function UserRow({ row }: { row: Extract<Row, { kind: "user" }> }) {
  const long = row.text.split("\n").length > FOLD_LINES || row.text.length > 1200;
  const [open, setOpen] = useState(false);
  return (
    // A message the agent took into its running turn looks like any other: that is how the agent itself
    // shows it. It stays where it landed, inside the turn.
    <div className="nt-user" data-steer={row.steer ? "1" : undefined}>
      <div className={cx("bubble", long && !open && "folded")}>
        <div className="txt">{row.text}</div>
        {long && (
          <button type="button" className="fold" onClick={() => setOpen((v) => !v)} aria-expanded={open}>
            {open ? t("hc_show_less") : t("hc_show_more")}
          </button>
        )}
      </div>
      <div className="meta">
        <CopyButton text={row.text} />
        <span className="tm tnum" title={new Date(row.ts).toLocaleString()}>
          {new Date(row.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
        </span>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Tools

const runRunning = (x: ToolItem) => x.status === "in_progress" || x.status === "pending";

function ToolDetail({ x }: { x: ToolItem }) {
  const [all, setAll] = useState(false);
  const cmd = toolCommand(x);
  const out = toolOutput(x);
  const shown = all ? { text: out.replace(/\n+$/, ""), hidden: 0 } : tailLines(out, 14);
  const input = x.input !== undefined && x.tool !== "execute" ? JSON.stringify(x.input, null, 2) : null;
  return (
    <div className="nt-tool-bd">
      {cmd && (
        <pre className="nt-cmd">
          <span className="ps">$ </span>
          <Highlighted code={cmd} lang="sh" />
        </pre>
      )}
      {input && input !== "{}" && !out && <CodeBlock code={input} lang="json" className="small" />}
      {x.locations.length > 0 && x.tool !== "execute" && (
        <div className="locs">
          {x.locations.map((l) => (
            <span key={l} className="mono" title={l}>
              {l}
            </span>
          ))}
        </div>
      )}
      {out && (
        <div className={cx("nt-out", x.status === "failed" && "failed")}>
          {shown.hidden > 0 && (
            <button type="button" className="more" onClick={() => setAll(true)}>
              {t("hc_show_all_lines", { n: shown.hidden + shown.text.split("\n").length })}
            </button>
          )}
          <pre>{shown.text}</pre>
          {all && out.split("\n").length > 14 && (
            <button type="button" className="more" onClick={() => setAll(false)}>
              {t("hc_show_less")}
            </button>
          )}
        </div>
      )}
      {x.truncated && <div className="cut">{t("hc_cut")}</div>}
    </div>
  );
}

/** One tool call: its kind, what it acted on, how long it took, and its output behind a fold. */
export function ToolLine({ x, now }: { x: ToolItem; now: number }) {
  const [open, setOpen] = useState(false);
  const { verb, target } = toolLabel(x);
  const Ic = KIND_ICON[x.tool] ?? Wrench;
  const running = runRunning(x);
  const took = (x.endedTs ?? now) - x.startedTs;
  const detail = x.content.length > 0 || toolCommand(x) !== null || (x.input !== undefined && JSON.stringify(x.input) !== "{}") || x.locations.length > 0;
  return (
    <div className={cx("nt-tl", x.stopped ? "stopped" : x.status, open && "open")}>
      <button type="button" className="rh" onClick={() => detail && setOpen((v) => !v)} aria-expanded={detail ? open : undefined} disabled={!detail}>
        <Ic size={14} className="ic" aria-hidden />
        {/* A command is its own label ("npm test"), as in Orca; other tools say what they did. */}
        {(x.tool !== "execute" || !target) && <span className="verb">{verb}</span>}
        {target && (
          <span className={cx("obj", x.tool === "execute" && "mono")} title={target}>
            {target}
          </span>
        )}
        <span className="end">
          {(running || took >= 1000) && <span className="tm tnum">{secs(Math.max(took, 0))}</span>}
          {running ? <Spinner /> : x.stopped ? <span className="cut-n">{t("hc_stopped")}</span> : x.status === "failed" ? <X size={13} className="bad" aria-label={t("hc_failed")} /> : null}
          {detail && <Chevron open={open} />}
        </span>
      </button>
      {open && <ToolDetail x={x} />}
    </div>
  );
}

/** A reasoning line inside an opened run. */
function ThoughtLine({ b }: { b: ThoughtBlock }) {
  const [open, setOpen] = useState(false);
  return (
    <div className={cx("nt-tl thought", open && "open")}>
      <button type="button" className="rh" onClick={() => setOpen((v) => !v)} aria-expanded={open}>
        <Brain size={14} className="ic" aria-hidden />
        <span className="verb">{t("hc_reasoning")}</span>
        <span className="obj faint">{inlineText(parseInline(b.text.split("\n")[0].replace(/^[#>\s]+/, "")))}</span>
        <span className="end">
          <Chevron open={open} />
        </span>
      </button>
      {open && <Markdown text={b.text} className="nt-thought-bd" />}
    </div>
  );
}

/** The run's sentence: "Ran 2 commands, read 1 file" (present tense while live). */
export function runSentence(tools: ToolItem[], live: boolean): string {
  const parts = runSummary(tools).map((p) => plural(`hc_${live ? "live" : "run"}_${p.cat}`, p.n));
  // Said once at the start where the language puts it there ("正在读 1 个文件，运行 2 条命令").
  const list = parts.length > 1 ? `${parts.slice(0, -1).join(t("hc_list_sep"))}${t("hc_list_and")}${parts[parts.length - 1]}` : parts[0] ?? "";
  const joined = (live ? t("hc_live_lead") : "") + list;
  return joined.charAt(0).toUpperCase() + joined.slice(1);
}

/** Tool calls in a row, summarized in one sentence that opens to the individual calls. */
export const ToolRun = memo(function ToolRun({ row, now }: { row: Extract<Row, { kind: "run" }>; now: number }) {
  const [open, setOpen] = useState(false);
  if (row.tools.length === 1 && row.members.length === 1) return isEdit(row.tools[0]) ? <EditRow tool={row.tools[0]} now={now} /> : <ToolLine x={row.tools[0]} now={now} />;
  const cats = runSummary(row.tools);
  const Ic = cats.length === 1 ? CAT_ICON[cats[0].cat] : Wrench;
  const { failed, stopped, done } = runOutcome(row.tools);
  const stat = runDiffStat(row.tools);
  const latest = row.live ? [...row.tools].reverse().find(runRunning) ?? row.tools[row.tools.length - 1] : null;
  return (
    <div className={cx("nt-run", open && "open", row.live && "is-live")}>
      <button type="button" className="rh" onClick={() => setOpen((v) => !v)} aria-expanded={open}>
        <Ic size={14} className="ic" aria-hidden />
        <span className="sent">{runSentence(row.tools, row.live)}</span>
        {stat && (
          <span className="stat tnum">
            <span className="add">+{stat.add}</span>
            <span className="del">−{stat.del}</span>
          </span>
        )}
        {failed > 0 && <span className="cnt">· {t("hc_failed_n", { n: failed })}</span>}
        {stopped > 0 && <span className="cnt">· {t("hc_stopped_n", { n: stopped })}</span>}
        {!row.live && done && failed === 0 && stopped === 0 && <Check size={12} className="ok" aria-hidden />}
        {latest && <span className="latest">{toolLabel(latest).target || latest.title}</span>}
        <Chevron open={open} />
      </button>
      {open && (
        <div className="members">
          {row.members.map((m) => (m.kind !== "tool" ? <ThoughtLine key={m.id} b={m} /> : isEdit(m) ? <EditRow key={m.callId} tool={m} now={now} /> : <ToolLine key={m.callId} x={m} now={now} />))}
        </div>
      )}
    </div>
  );
});

// ---------------------------------------------------------------------------------------------
// Edits

function DiffBody({ rows }: { rows: DiffRow[] }) {
  return (
    <div className="nt-diff-rows" tabIndex={0}>
      {rows.map((r, i) =>
        r.kind === "gap" ? (
          <div key={i} className="gap" role="separator">
            {t("hc_gap_lines", { n: r.lines })}
          </div>
        ) : (
          <div key={i} className={cx("ln", r.kind)}>
            <span className="sg" aria-hidden>
              {r.kind === "add" ? "+" : r.kind === "del" ? "-" : " "}
            </span>
            <span className="tx">{r.text || " "}</span>
          </div>
        ),
      )}
    </div>
  );
}

function DiffFile({ d, status }: { d: Extract<ToolContent, { type: "diff" }>; status: ToolItem["status"] }) {
  const [open, setOpen] = useState(false);
  const rows = useMemo(() => diffRows(d.old, d.new), [d.old, d.new]);
  const stat = useMemo(() => diffStat([d]), [d]);
  const created = d.old === null || d.old === "";
  const deleted = d.new === "" && !created;
  const Ic = created ? FilePlus2 : deleted ? FileMinus2 : FilePen;
  return (
    <div className={cx("nt-diff", open && "open")}>
      <div className="row">
        <button type="button" className="rh" onClick={() => rows.length && setOpen((v) => !v)} aria-expanded={rows.length ? open : undefined}>
          <Ic size={14} className="ic" aria-hidden />
          <span className="verb">{created ? t("hc_created") : deleted ? t("hc_deleted") : t("hc_edited")}</span>
          <span className="obj" title={d.path}>
            {baseName(d.path)}
          </span>
          {stat && (
            <span className="stat tnum">
              <span className="add">+{stat.add}</span>
              <span className="del">−{stat.del}</span>
            </span>
          )}
          {status === "failed" && <span className="bad">{t("hc_failed")}</span>}
          {(status === "in_progress" || status === "pending") && <Spinner />}
          {rows.length > 0 && <Chevron open={open} />}
        </button>
        <CopyButton text={patchText(d.path, rows)} label={t("hc_copy_diff")} />
      </div>
      {open && <DiffBody rows={rows} />}
    </div>
  );
}

/** An edit the agent made, as one card per file. */
export const EditRow = memo(function EditRow({ tool, now }: { tool: ToolItem; now: number }) {
  const diffs = tool.content.filter((c): c is Extract<ToolContent, { type: "diff" }> => c.type === "diff");
  if (!diffs.length) return <ToolLine x={tool} now={now} />;
  return (
    <div className="nt-edits">
      {diffs.map((d, i) => (
        <DiffFile key={`${d.path}:${i}`} d={d} status={tool.status} />
      ))}
      {tool.truncated && <div className="cut">{t("hc_cut")}</div>}
    </div>
  );
});

// ---------------------------------------------------------------------------------------------
// Reasoning, plans, sub-agents

export const ThoughtRow = memo(function ThoughtRow({ block, live, now }: { block: ThoughtBlock; live: boolean; now: number }) {
  const [open, setOpen] = useState(false);
  if (!block.text.trim() && !live) return null;
  const took = (live ? now : block.endedTs) - block.startedTs;
  return (
    <div className={cx("nt-reason", open && "open", live && "is-live")}>
      <button type="button" className="rh" onClick={() => setOpen((v) => !v)} aria-expanded={open}>
        <Brain size={14} className="ic" aria-hidden />
        <span className="sent">{live ? t("hc_thinking") : took >= 1000 ? t("hc_thought", { s: secs(took) }) : t("hc_reasoning")}</span>
        <Chevron open={open} />
      </button>
      {open && <Markdown text={block.text} className="nt-thought-bd" />}
    </div>
  );
});

export function PlanRow({ entries }: { entries: PlanEntry[] }) {
  const done = entries.filter((e) => e.status === "completed").length;
  return (
    <div className="nt-plan">
      <div className="rh">
        <ListTodo size={14} className="ic" aria-hidden />
        <span>{t("hc_plan")}</span>
        <span className="n tnum">{t("hc_plan_progress", { done, n: entries.length })}</span>
      </div>
      <ul>
        {entries.map((e, i) => (
          <li key={i} className={e.status}>
            {e.status === "completed" ? <CircleCheck size={14} aria-hidden /> : e.status === "in_progress" ? <CircleDot size={14} aria-hidden /> : <Circle size={14} aria-hidden />}
            <span>{e.content}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

export const AgentRow = memo(function AgentRow({ tool, now }: { tool: ToolItem; now: number }) {
  const [open, setOpen] = useState(false);
  const running = runRunning(tool);
  const took = (tool.endedTs ?? now) - tool.startedTs;
  const out = toolOutput(tool);
  return (
    <div className={cx("nt-sub", open && "open", running && "is-live")}>
      <button type="button" className="rh" onClick={() => out && setOpen((v) => !v)} aria-expanded={out ? open : undefined}>
        <Bot size={14} className="ic" aria-hidden />
        <span className="verb">{t("hc_subagent")}</span>
        <span className="obj">{toolLabel(tool).target || tool.title}</span>
        <span className="end">
          {(running || took >= 1000) && <span className="tm tnum">{secs(took)}</span>}
          {running ? <Spinner /> : tool.status === "failed" ? <X size={13} className="bad" /> : <Check size={12} className="ok" />}
          {out && <Chevron open={open} />}
        </span>
      </button>
      {open && <Markdown text={out} className="nt-sub-bd" />}
    </div>
  );
});

// ---------------------------------------------------------------------------------------------
// Questions and notices

export function QuestionCard({ block, onAnswer, readonly }: { block: QuestionBlock; onAnswer: (qid: string, text: string) => void; readonly: boolean }) {
  const [text, setText] = useState("");
  const send = (v: string) => v.trim() && onAnswer(block.qid, v.trim());
  // A number key picks that choice, unless the reader is typing somewhere.
  useEffect(() => {
    if (readonly || block.answered || !block.options.length) return;
    const onKey = (e: KeyboardEvent) => {
      const el = document.activeElement as HTMLElement | null;
      if (e.metaKey || e.ctrlKey || e.altKey || e.isComposing || (el && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName)))) return;
      const n = Number(e.key);
      if (n >= 1 && n <= block.options.length) {
        e.preventDefault();
        onAnswer(block.qid, block.options[n - 1]);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [readonly, block.answered, block.options, block.qid, onAnswer]);
  if (block.answered) {
    return (
      <div className="nt-q done">
        <MessageCircleQuestion size={14} className="ic" aria-hidden />
        <span className="prompt">{inlineText(parseInline(block.prompt))}</span>
        <span className="ans">{block.answer ? t("hc_answered", { a: block.answer }) : t("hc_answered_plain")}</span>
      </div>
    );
  }
  return (
    <div className="nt-q" role="group" aria-label={t("hc_question")}>
      <div className="top">
        <MessageCircleQuestion size={14} className="ic" aria-hidden />
        <span className="lbl">{t("hc_question")}</span>
      </div>
      <Markdown text={block.prompt} className="prompt" />
      {!readonly && (
        <>
          {block.options.length > 0 && (
            <div className="opts">
              {block.options.map((o, i) => (
                <button key={o} type="button" className="opt" onClick={() => send(o)}>
                  <span className="k tnum">{i + 1}</span>
                  <span>{o}</span>
                </button>
              ))}
            </div>
          )}
          <div className="other">
            <input
              value={text}
              placeholder={block.options.length ? t("hc_other_answer") : t("hc_answer_ph")}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key !== "Enter" || e.nativeEvent.isComposing || e.keyCode === 229) return;
                e.preventDefault();
                send(text);
              }}
            />
            <button type="button" className="btn sm primary" disabled={!text.trim()} onClick={() => send(text)}>
              {t("hc_send_answer")}
            </button>
          </div>
        </>
      )}
    </div>
  );
}

/** Text for a notice: ours for the codes the panel knows, the host's otherwise. */
export function noticeText(code: string, text: string): string {
  if (code === "steer_queued") return t("hc_steer_queued");
  return text;
}

export function NoticeRow({ level, code, text }: { level: string; code: string; text: string }) {
  const Ic = level === "error" ? CircleAlert : level === "warn" ? TriangleAlert : Info;
  return (
    <div className={cx("nt-notice", level)}>
      <Ic size={13} aria-hidden />
      <span>{noticeText(code, text)}</span>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// The end of a turn

/** Under a finished turn: how it ended, how long it took, and copying the reply. */
export function TurnFoot({ turn, reply, stopped, onContinue, readonly }: { turn: Turn; reply: string; stopped: boolean; onContinue: () => void; readonly: boolean }) {
  if (turn.status === "running") return null;
  const tokens = turn.usage?.output != null ? Math.round(((turn.usage.input ?? 0) + turn.usage.output) / 100) / 10 : null;
  const meta = [turn.durationMs != null ? secs(turn.durationMs) : null, tokens != null ? t("hc_tokens", { n: tokens }) : null].filter(Boolean).join(" · ");
  return (
    <>
      {turn.status === "failed" && (
        <div className="nt-notice error">
          <CircleAlert size={13} aria-hidden />
          <span>
            <b>{t("hc_failed")}</b>
            {turn.reason ? `: ${turn.reason}` : ""}
          </span>
        </div>
      )}
      {turn.status === "interrupted" && stopped && !readonly ? (
        <div className="nt-stopped">
          <span>{turn.reason ? `${t("hc_stopped_lede")} ${turn.reason}` : t("hc_stopped_lede")}</span>
          <button type="button" className="btn sm" onClick={onContinue}>
            <Play size={12} aria-hidden />
            {t("hc_continue")}
          </button>
        </div>
      ) : (
        <div className={cx("nt-foot", turn.status)}>
          {turn.status === "interrupted" && <span className="st">{t("hc_stopped")}</span>}
          {reply && <CopyButton text={reply} label={t("hc_copy_reply")} />}
          {meta && <span className="meta tnum">{meta}</span>}
        </div>
      )}
    </>
  );
}

/** While the turn runs: what the agent does now, and for how long. */
export function WorkingRow({ label, since, now }: { label: string; since?: number; now: number }) {
  return (
    <div className="nt-working" aria-live="polite">
      <i className="pulse" aria-hidden />
      <span className="shimmer">{label}</span>
      {since !== undefined && <span className="tm tnum">{secs(Math.max(0, now - since))}</span>}
    </div>
  );
}

export function RowView({ row, now, readonly, onAnswer }: { row: Row; now: number; readonly: boolean; onAnswer: (qid: string, text: string) => void }): ReactNode {
  switch (row.kind) {
    case "user":
      return <UserRow row={row} />;
    case "text":
      return <Markdown text={row.text} className={cx("nt-reply", row.streaming && "streaming")} />;
    case "thought":
      return <ThoughtRow block={row.block} live={row.live} now={now} />;
    case "run":
      return <ToolRun row={row} now={now} />;
    case "agent":
      return <AgentRow tool={row.tool} now={now} />;
    case "plan":
      return <PlanRow entries={row.entries} />;
    case "question":
      return <QuestionCard block={row.block} onAnswer={onAnswer} readonly={readonly} />;
    case "notice":
      return <NoticeRow level={row.level} code={row.code} text={row.text} />;
  }
}
