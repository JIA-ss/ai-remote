// A Codex turn, laid out the way Codex's own desktop app shows one (lib/codex.ts has the structure):
// the user's message on the right; the agent's work as quiet one-line units that open in place — a
// command opens to its shell output, an edit to its diff with line numbers — with what the agent said
// between them; "Worked for …" folding all of that once the answer starts; the answer; and the files
// the turn changed, each opening to its diff. The look was studied from Codex's app; the components
// are this project's own, on this app's Markdown, previews and question card.

import { ChevronRight, CircleCheck, Circle, CircleDot, FileDiff, ListTodo, Play } from "lucide-react";
import { memo, useEffect, useMemo, useState, type ReactNode } from "react";
import { baseName, type PlanEntry, type ThoughtBlock, type ToolItem, type Turn } from "../../../lib/chat";
import {
  activeAct,
  codexTurn,
  duration,
  elapsed,
  groupSummary,
  planStep,
  relPath,
  workedMs,
  type Act,
  type CmdAction,
  type DiffLine,
  type ExecInfo,
  type FileChange,
  type Unit,
} from "../../../lib/codex";
import { currentLang, t, type Key } from "../../../lib/i18n";
import { inlineText, parseInline } from "../../../lib/markdown";
import { cx } from "../../util";
import { CodeBlock, CopyButton, Highlighted, Markdown } from "../Markdown";
import { PreviewButton, TurnFiles } from "../Preview";
import { NoticeRow, QuestionCard, UserRow } from "../Rows";

const zh = () => currentLang() === "zh";
const plural = (key: string, n: number, vars: Record<string, string | number> = {}) => t(`${key}_${n === 1 ? "one" : "many"}` as Key, { n, ...vars });
const toolRunning = (x: ToolItem) => x.status === "in_progress" || x.status === "pending";

/** Text that shimmers while the agent works on it. */
function Shimmer({ children, on }: { children: ReactNode; on: boolean }) {
  return <span className={cx(on && "cdx-shimmer")}>{children}</span>;
}

function Chevron({ open }: { open: boolean }) {
  return <ChevronRight size={12} className={cx("cdx-chev", open && "open")} aria-hidden />;
}

/** Opens and closes its body with a height animation (Codex's activity disclosure). */
function Reveal({ open, children, className }: { open: boolean; children: ReactNode; className?: string }) {
  const [shown, setShown] = useState(open);
  useEffect(() => {
    if (open) setShown(true);
    else {
      const timer = setTimeout(() => setShown(false), 160);
      return () => clearTimeout(timer);
    }
  }, [open]);
  if (!open && !shown) return null;
  return <div className={cx("cdx-reveal", open ? "x-in" : "x-out", className)}>{children}</div>;
}

/** A one-line activity: its words, and (when it has any) details that open below it. */
function ActLine({ label, live, failed, details, defaultOpen = false, className, trailing }: {
  label: ReactNode;
  live?: boolean;
  failed?: boolean;
  details?: (() => ReactNode) | null;
  defaultOpen?: boolean;
  className?: string;
  trailing?: ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const can = !!details;
  return (
    <div className={cx("cdx-act", open && "open", live && "is-live", failed && "failed", className)}>
      <div className="x-hd">
        <button type="button" className="x-ln" onClick={() => can && setOpen((v) => !v)} aria-expanded={can ? open : undefined} disabled={!can}>
          <span className="x-tx"><Shimmer on={!!live}>{label}</Shimmer></span>
          {can && <Chevron open={open} />}
        </button>
        {trailing}
      </div>
      {can && <Reveal open={open} className="x-bd">{details!()}</Reveal>}
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Words for one act

function Verb({ k }: { k: Key }) {
  return <span className="x-v">{t(k)}</span>;
}

function Detail({ children }: { children: ReactNode }) {
  return <span className="x-d">{children}</span>;
}

function actionWords(a: CmdAction, done: boolean, cwd: string | null): ReactNode {
  switch (a.type) {
    case "read":
      return (
        <>
          <Verb k={done ? "cdx_v_read" : "cdx_v_reading"} /> <Detail><span className="x-path" title={a.path}>{relPath(a.path, cwd) || a.name}</span></Detail>
        </>
      );
    case "search":
      return (
        <>
          <Verb k={done ? "cdx_v_searched" : "cdx_v_searching"} />{" "}
          <Detail>
            {a.query ? t("cdx_d_for", { q: a.query }) : t("cdx_d_files")}
            {a.path ? ` ${t("cdx_d_in", { p: relPath(a.path, cwd) })}` : ""}
          </Detail>
        </>
      );
    case "list_files":
      return (
        <>
          <Verb k={done ? "cdx_v_listed" : "cdx_v_listing"} /> <Detail>{a.path ? t("cdx_d_files_in", { p: relPath(a.path, cwd) }) : t("cdx_d_files")}</Detail>
        </>
      );
    default:
      return null;
  }
}

function execWords(info: ExecInfo, x: ToolItem, cwd: string | null): ReactNode {
  const done = !toolRunning(x);
  if (info.explore) {
    return info.actions.map((a, i) => (
      <span key={i} className="x-part">
        {i > 0 && <span className="x-sep">, </span>}
        {actionWords(a, done, info.cwd ?? cwd)}
      </span>
    ));
  }
  const took = done ? elapsed(info.durationMs, zh()) : null;
  return (
    <>
      <Verb k={!done ? "cdx_v_running" : x.stopped ? "cdx_v_stopped" : "cdx_v_ran"} /> <code className="x-cmd">{oneLine(info.command)}</code>
      {took && <Detail> {t("cdx_in_time", { t: took })}</Detail>}
      {done && !x.stopped && info.exitCode !== null && info.exitCode !== 0 && <span className="x-bad"> · {t("cdx_exit_short", { n: info.exitCode })}</span>}
    </>
  );
}

function oneLine(s: string): string {
  const first = s.split("\n")[0];
  return first.length < s.length ? `${first} …` : first;
}

function fileVerb(f: FileChange, done: boolean): Key {
  if (f.kind === "add") return done ? "cdx_v_created" : "cdx_v_creating";
  if (f.kind === "delete") return done ? "cdx_v_deleted" : "cdx_v_deleting";
  if (f.movePath && done) return "cdx_v_moved";
  return done ? "cdx_v_edited" : "cdx_v_editing";
}

function Stat({ add, del }: { add: number; del: number }) {
  if (!add && !del) return null;
  return (
    <span className="cdx-stat tnum">
      {add > 0 && <span className="x-add">+{add}</span>}
      {del > 0 && <span className="x-del">−{del}</span>}
    </span>
  );
}

function fileWords(f: FileChange, x: ToolItem, cwd: string | null): ReactNode {
  const done = !toolRunning(x);
  const failed = x.status === "failed" && !x.stopped;
  return (
    <>
      <Verb k={failed ? "cdx_v_failed" : fileVerb(f, done)} />{" "}
      <Detail>
        <span className="x-path" title={relPath(f.path, cwd)}>{baseName(f.path)}</span>
        {f.movePath && <> {t("cdx_d_to", { p: baseName(f.movePath) })}</>}
      </Detail>{" "}
      <Stat add={f.add} del={f.del} />
    </>
  );
}

function toolName(x: ToolItem): string {
  return x.title.trim() || "tool";
}

/** The words of an act as its line says them (present tense while it runs). */
function actWords(a: Act, cwd: string | null): ReactNode {
  switch (a.kind) {
    case "exec":
      return execWords(a.info, a.tool, cwd);
    case "patch":
      if (a.files.length === 1) return fileWords(a.files[0], a.tool, cwd);
      return (
        <>
          <Verb k={toolRunning(a.tool) ? "cdx_v_editing" : "cdx_v_edited"} /> <Detail>{a.files.map((f) => baseName(f.path)).join(", ")}</Detail>{" "}
          <Stat add={a.files.reduce((n, f) => n + f.add, 0)} del={a.files.reduce((n, f) => n + f.del, 0)} />
        </>
      );
    case "web":
      return (
        <>
          <Verb k={toolRunning(a.tool) ? "cdx_v_web_ing" : "cdx_v_web"} />
          {a.query && <Detail> {t("cdx_d_for", { q: a.query })}</Detail>}
        </>
      );
    case "tool":
      return (
        <>
          <Verb k={toolRunning(a.tool) ? "cdx_v_calling" : a.tool.status === "failed" ? "cdx_v_failed" : "cdx_v_called"} /> <Detail><code className="x-cmd">{toolName(a.tool)}</code></Detail>
        </>
      );
    case "thought":
      return (
        <>
          <Verb k="cdx_thought" /> <Detail>{inlineText(parseInline(a.block.text.trim().split("\n")[0].replace(/^[#>\s*_]+|[*_]+$/g, "")))}</Detail>
        </>
      );
  }
}

// ---------------------------------------------------------------------------------------------
// Details: a command's shell, an edit's diff, a tool's input and output

/** Lines of output shown before "N more lines". */
const OUTPUT_LINES = 40;

function ShellCard({ info, x }: { info: ExecInfo; x: ToolItem }) {
  const [all, setAll] = useState(false);
  const out = info.output.replace(/\n+$/, "");
  const lines = out ? out.split("\n") : [];
  const shown = all || lines.length <= OUTPUT_LINES ? out : lines.slice(-OUTPUT_LINES).join("\n");
  const running = toolRunning(x);
  return (
    <div className="cdx-shell">
      <div className="x-sh-hd">
        <span>{t("cdx_shell")}</span>
        <span className="x-acts">
          <CopyButton text={info.command} label={t("cdx_copy_cmd")} />
          {out && <CopyButton text={out} label={t("cdx_copy_output")} />}
        </span>
      </div>
      <div className="x-sh-bd">
        <pre className="x-cmd"><span className="x-ps">$ </span><Highlighted code={info.command} lang="sh" /></pre>
        {!all && lines.length > OUTPUT_LINES && (
          <button type="button" className="x-more" onClick={() => setAll(true)}>{t("cdx_more_lines", { n: lines.length - OUTPUT_LINES })}</button>
        )}
        {out ? <pre className="x-out">{shown}</pre> : !running && <div className="x-none">{t("cdx_no_output")}</div>}
        {x.truncated && <div className="x-none">{t("hc_cut")}</div>}
      </div>
      <div className={cx("x-sh-ft", !running && !x.stopped && info.exitCode !== null && info.exitCode !== 0 && "x-bad")}>
        {running ? <Shimmer on>{t("cdx_v_running")}</Shimmer>
          : x.stopped ? t("cdx_stopped")
            : info.exitCode === 0 ? <><CircleCheck size={12} aria-hidden /> {t("cdx_success")}</>
              : info.exitCode !== null ? t("cdx_exit_code", { n: info.exitCode })
                : x.status === "failed" ? t("hc_failed") : null}
      </div>
    </div>
  );
}

/** Most diff lines drawn at once; the rest open on request. */
const DIFF_LINES = 400;

export function DiffView({ lines }: { lines: DiffLine[] }) {
  const [all, setAll] = useState(false);
  const shown = all ? lines : lines.slice(0, DIFF_LINES);
  if (!lines.length) return <div className="cdx-diff x-empty">{t("cdx_too_large")}</div>;
  return (
    <div className="cdx-diff" tabIndex={0}>
      {shown.map((l, i) =>
        l.kind === "hunk" ? (
          <div key={i} className="x-hunk" role="separator">{l.text.replace(/^@@[^@]*@@\s?/, "") || "⋯"}</div>
        ) : (
          <div key={i} className={cx("x-dl", `x-${l.kind}`)}>
            <span className="x-no x-o">{l.old ?? ""}</span>
            <span className="x-no x-n">{l.new ?? ""}</span>
            <span className="x-sg" aria-hidden>{l.kind === "add" ? "+" : l.kind === "del" ? "−" : " "}</span>
            <span className="x-tx">{l.text || " "}</span>
          </div>
        ),
      )}
      {!all && lines.length > DIFF_LINES && (
        <button type="button" className="x-more" onClick={() => setAll(true)}>{t("cdx_more_lines", { n: lines.length - DIFF_LINES })}</button>
      )}
    </div>
  );
}

function patchOf(f: FileChange, cwd: string | null): string {
  const p = relPath(f.path, cwd);
  const body = f.lines.map((l) => (l.kind === "hunk" ? l.text || "@@" : `${l.kind === "add" ? "+" : l.kind === "del" ? "-" : " "}${l.text}`));
  return [`--- ${f.kind === "add" ? "/dev/null" : `a/${p}`}`, `+++ ${f.kind === "delete" ? "/dev/null" : `b/${f.movePath ? relPath(f.movePath, cwd) : p}`}`, ...body].join("\n");
}

function ToolDetails({ x }: { x: ToolItem }) {
  const input = x.input !== undefined && JSON.stringify(x.input) !== "{}" ? JSON.stringify(x.input, null, 2) : null;
  const out = x.content.filter((c): c is { type: "text"; text: string } => c.type === "text").map((c) => c.text).join("\n").trim();
  return (
    <div className="cdx-tool">
      {input && <CodeBlock code={input} lang="json" className="small" />}
      {out && <pre className="x-out">{out}</pre>}
    </div>
  );
}

function hasToolDetails(x: ToolItem): boolean {
  return (x.input !== undefined && JSON.stringify(x.input) !== "{}") || x.content.some((c) => c.type === "text" && c.text.trim());
}

/** One act as a line of its own (inside a group, or a group of one). */
function ActRow({ a, cwd }: { a: Act; cwd: string | null }): ReactNode {
  switch (a.kind) {
    case "exec":
      return <ActLine label={actWords(a, cwd)} live={toolRunning(a.tool)} failed={a.tool.status === "failed" && !a.tool.stopped}
        details={() => <ShellCard info={a.info} x={a.tool} />} />;
    case "patch":
      if (a.files.length <= 1) {
        const f = a.files[0];
        return <ActLine label={actWords(a, cwd)} live={toolRunning(a.tool)} failed={a.tool.status === "failed" && !a.tool.stopped}
          details={f?.lines.length ? () => <DiffView lines={f.lines} /> : null}
          trailing={f && <FileButtons f={f} cwd={cwd} x={a.tool} />} />;
      }
      return (
        <>
          {a.files.map((f) => (
            <ActLine key={f.path} label={fileWords(f, a.tool, cwd)} live={toolRunning(a.tool)} failed={a.tool.status === "failed" && !a.tool.stopped}
              details={f.lines.length ? () => <DiffView lines={f.lines} /> : null} trailing={<FileButtons f={f} cwd={cwd} x={a.tool} />} />
          ))}
        </>
      );
    case "web":
      return <ActLine label={actWords(a, cwd)} live={toolRunning(a.tool)} details={hasToolDetails(a.tool) ? () => <ToolDetails x={a.tool} /> : null} />;
    case "tool":
      return <ActLine label={actWords(a, cwd)} live={toolRunning(a.tool)} failed={a.tool.status === "failed" && !a.tool.stopped}
        details={hasToolDetails(a.tool) ? () => <ToolDetails x={a.tool} /> : null} />;
    case "thought":
      return <ActLine className="thought" label={actWords(a, cwd)} details={a.block.text.includes("\n") || a.block.text.length > 80 ? () => <Markdown text={a.block.text} className="cdx-thought-md" /> : null} />;
  }
}

function FileButtons({ f, cwd, x }: { f: FileChange; cwd: string | null; x: ToolItem }) {
  if (x.status === "failed" || toolRunning(x)) return null;
  return (
    <span className="x-btns">
      {f.kind !== "delete" && <PreviewButton path={f.movePath ?? f.path} />}
      {f.lines.length > 0 && <CopyButton text={patchOf(f, cwd)} label={t("cdx_copy_patch")} />}
    </span>
  );
}

// ---------------------------------------------------------------------------------------------
// Units

function summarySentence(acts: Act[]): ReactNode {
  const s = groupSummary(acts);
  const words = s.parts.map((p, i) => plural(`cdx_s_${p.part}${i === 0 ? "_lead" : ""}`, p.n));
  let text = words.join(", ");
  try {
    text = new Intl.ListFormat(zh() ? "zh-CN" : "en", { type: "conjunction" }).format(words);
  } catch {
    // An engine without ListFormat keeps the plain list.
  }
  return (
    <>
      {text} <Stat add={s.add} del={s.del} />
    </>
  );
}

/** Tool calls in a row: one line naming what runs now, or what was done; opens to each call. */
const GroupRow = memo(function GroupRow({ u, cwd }: { u: Extract<Unit, { kind: "group" }>; cwd: string | null }) {
  const acts = u.acts.filter((a) => a.kind !== "thought");
  if (!u.live && acts.length === 1 && !(acts[0].kind === "patch" && acts[0].files.length > 1)) return <ActRow a={acts[0]} cwd={cwd} />;
  // Live: the call running now; between calls, the reading the agent is in the middle of, or "Thinking".
  const active = u.live ? activeAct(u.acts) : null;
  const label = !u.live
    ? summarySentence(u.acts)
    : active && active.kind !== "thought" && (toolRunning(active.tool) || (active.kind === "exec" && active.info.explore))
      ? actWords(active, cwd)
      : t("cdx_thinking");
  const failed = acts.some((a) => a.tool.status === "failed" && !a.tool.stopped);
  return (
    <ActLine className="group" label={label} live={u.live} failed={!u.live && failed}
      details={() => (
        <div className="cdx-list">
          {u.acts.map((a) => <ActRow key={a.key} a={a} cwd={cwd} />)}
        </div>
      )} />
  );
});

function ThoughtRow({ block, live, now }: { block: ThoughtBlock; live: boolean; now: number }) {
  const took = (live ? now : block.endedTs) - block.startedTs;
  const label = live ? t("cdx_thinking") : took >= 1000 ? t("cdx_thought_for", { t: duration(took, zh()) }) : t("cdx_thought");
  return <ActLine className="thought" label={label} live={live} details={block.text.trim() ? () => <Markdown text={block.text} className="cdx-thought-md" /> : null} />;
}

function AgentRow({ x }: { x: ToolItem }) {
  const out = x.content.filter((c): c is { type: "text"; text: string } => c.type === "text").map((c) => c.text).join("\n").trim();
  const prompt = (x.input as { prompt?: unknown } | undefined)?.prompt;
  return (
    <ActLine label={<><Verb k="cdx_subagent" /> <Detail>{x.title.replace(/^agent\s*/i, "")}</Detail></>} live={toolRunning(x)} failed={x.status === "failed"}
      details={out || typeof prompt === "string" ? () => (
        <div className="cdx-tool">
          {typeof prompt === "string" && <Markdown text={prompt} className="cdx-thought-md" />}
          {out && <Markdown text={out} />}
        </div>
      ) : null} />
  );
}

function PlanCard({ entries, live }: { entries: PlanEntry[]; live: boolean }) {
  // Open while the turn runs, folded once it is done, unless the reader chose otherwise.
  const [chosen, setChosen] = useState<boolean | null>(null);
  const open = chosen ?? live;
  const st = planStep(entries);
  return (
    <div className={cx("cdx-plan", open && "open")}>
      <button type="button" className="x-hd" onClick={() => setChosen(!open)} aria-expanded={open}>
        <ListTodo size={13} aria-hidden />
        <span className="x-ti">{t("cdx_plan")}</span>
        <span className="x-pill tnum">{st.done === st.total ? t("cdx_plan_done", { d: st.done, n: st.total }) : t("cdx_plan_step", { s: st.step, n: st.total })}</span>
        {!open && st.current && st.done < st.total && <span className="x-cur">{st.current.content}</span>}
        <Chevron open={open} />
      </button>
      <Reveal open={open}>
        <ol>
          {entries.map((e, i) => (
            <li key={i} className={e.status}>
              {e.status === "completed" ? <CircleCheck size={13} aria-hidden /> : e.status === "in_progress" ? <CircleDot size={13} aria-hidden /> : <Circle size={13} aria-hidden />}
              <span>{e.content}</span>
            </li>
          ))}
        </ol>
      </Reveal>
    </div>
  );
}

function UnitView({ u, cwd, now, readonly, onAnswer }: { u: Unit; cwd: string | null; now: number; readonly: boolean; onAnswer: (qid: string, text: string) => void }): ReactNode {
  switch (u.kind) {
    case "group":
      return <GroupRow u={u} cwd={cwd} />;
    case "message":
      return <Markdown text={u.block.text} className={cx("cdx-md", u.streaming && "streaming")} />;
    case "thought":
      return <ThoughtRow block={u.block} live={u.live} now={now} />;
    case "agent":
      return <AgentRow x={u.tool} />;
    case "question":
      return <QuestionCard block={u.block} onAnswer={onAnswer} readonly={readonly} />;
    case "notice":
      if (u.code === "compacted") return <div className="cdx-divider"><span>{t("cdx_compacted")}</span></div>;
      return <NoticeRow level={u.level} code={u.code} text={u.text} />;
    case "steer":
      return <UserRow row={{ kind: "user", key: u.key, text: u.block.text, by: u.block.by, ts: u.block.ts, steer: true, inputId: u.block.inputId, attachments: u.block.attachments }} />;
  }
}

// ---------------------------------------------------------------------------------------------
// The changes a turn made

/** Files shown before "Show N more files". */
const FILES_SHOWN = 6;

function ChangesCard({ files, cwd }: { files: FileChange[]; cwd: string | null }) {
  const [more, setMore] = useState(false);
  const add = files.reduce((n, f) => n + f.add, 0);
  const del = files.reduce((n, f) => n + f.del, 0);
  const shown = more ? files : files.slice(0, FILES_SHOWN);
  return (
    <div className="cdx-changes">
      <div className="x-hd">
        <FileDiff size={13} aria-hidden />
        <span className="x-ti">{plural("cdx_files_changed", files.length)}</span>
        <Stat add={add} del={del} />
        <span className="x-acts">
          <CopyButton text={files.map((f) => patchOf(f, cwd)).join("\n")} label={t("cdx_copy_patch")} />
        </span>
      </div>
      {shown.map((f) => <ChangedFile key={f.path} f={f} cwd={cwd} />)}
      {!more && files.length > FILES_SHOWN && (
        <button type="button" className="x-more" onClick={() => setMore(true)}>{plural("cdx_show_more_files", files.length - FILES_SHOWN)}</button>
      )}
    </div>
  );
}

function ChangedFile({ f, cwd }: { f: FileChange; cwd: string | null }) {
  const [open, setOpen] = useState(false);
  const rel = relPath(f.movePath ?? f.path, cwd);
  return (
    <div className={cx("x-file", open && "open")}>
      <div className="x-row">
        <button type="button" className="x-ln" onClick={() => f.lines.length && setOpen((v) => !v)} aria-expanded={f.lines.length ? open : undefined}>
          <Chevron open={open} />
          <span className="x-path" title={rel}>
            <span className="x-dir">{rel.slice(0, rel.length - baseName(rel).length)}</span>
            <span className="x-nm">{baseName(rel)}</span>
          </span>
          {f.kind === "add" && <span className="x-tag">A</span>}
          {f.kind === "delete" && <span className="x-tag x-del">D</span>}
          <Stat add={f.add} del={f.del} />
        </button>
        {f.kind !== "delete" && <PreviewButton path={f.movePath ?? f.path} />}
      </div>
      <Reveal open={open}><DiffView lines={f.lines} /></Reveal>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// The turn

/** Folded turns, by session and turn, so a turn opened once stays open while the panel lives. */
const opened = new Map<string, boolean>();

export const CodexTurnView = memo(function CodexTurnView({ turn, session, now, readonly, stopped, cwd, far, onAnswer, onContinue }: {
  turn: Turn;
  session: string;
  now: number;
  readonly: boolean;
  /** The last turn, stopped: it offers to continue. */
  stopped: boolean;
  cwd: string | null;
  far: boolean;
  onAnswer: (qid: string, text: string) => void;
  onContinue: () => void;
}) {
  const v = useMemo(() => codexTurn(turn), [turn]);
  const key = `${session}:${turn.n}`;
  const [open, setOpen] = useState<boolean>(() => opened.get(key) ?? false);
  const folded = v.foldable && !open;
  const ms = workedMs(turn, now);
  const time = duration(ms, zh());
  const answerText = v.answer?.block.text ?? "";
  const tokens = turn.usage?.output != null ? Math.round(((turn.usage.input ?? 0) + turn.usage.output) / 100) / 10 : null;
  const work = v.units;
  const toggle = () => {
    opened.set(key, !open);
    setOpen(!open);
  };
  return (
    <section className={cx("cdx-turn", turn.status === "running" && "running", far && "far")} data-turn={turn.n}>
      {turn.user && (
        <UserRow row={{ kind: "user", key: `u${turn.n}`, text: turn.user.text, by: turn.user.by, ts: turn.user.ts, steer: false, inputId: turn.user.inputId, attachments: turn.user.attachments }} />
      )}
      {v.plan && <PlanCard entries={v.plan} live={turn.status === "running"} />}
      {v.foldable && (
        <div className="cdx-worked">
          <button type="button" className="x-tg" onClick={toggle} aria-expanded={open}>
            <span className="tnum">{t("cdx_worked_for", { t: time })}</span>
            <Chevron open={open} />
          </button>
          <div className="x-rule" />
        </div>
      )}
      <Reveal open={!folded} className="cdx-work">
        {work.map((u) => (
          <div key={u.key} className="cdx-unit" data-unit={u.kind}>
            <UnitView u={u} cwd={cwd} now={now} readonly={readonly} onAnswer={onAnswer} />
          </div>
        ))}
      </Reveal>
      {v.thinking && <div className="cdx-thinking"><Shimmer on>{t("cdx_thinking")}</Shimmer></div>}
      {v.status === "working" && !v.answer && (
        <div className="cdx-status tnum" aria-live="polite">
          {v.asking ? t("cdx_waiting_answer") : ms >= 1000 ? t("cdx_working_for", { t: time }) : t("cdx_working")}
        </div>
      )}
      {v.answer && <Markdown text={answerText} className={cx("cdx-md cdx-answer", v.answer.streaming && "streaming")} />}
      {v.status === "stopped" && <div className="cdx-divider x-stop"><span className="tnum">{t("cdx_stopped_after", { t: time })}</span></div>}
      {v.status === "failed" && (
        <NoticeRow level="error" code="failed" text={turn.reason ? `${t("hc_failed")}: ${turn.reason}` : t("hc_failed")} />
      )}
      {v.changes.length > 0 && <ChangesCard files={v.changes} cwd={cwd} />}
      {turn.status !== "running" && <TurnFiles turn={turn} />}
      {turn.status !== "running" && (stopped && !readonly ? (
        <div className="nt-stopped">
          <span>{turn.reason ? `${t("hc_stopped_lede")} ${turn.reason}` : t("hc_stopped_lede")}</span>
          <button type="button" className="btn sm" onClick={onContinue}>
            <Play size={12} aria-hidden />
            {t("hc_continue")}
          </button>
        </div>
      ) : (answerText || tokens != null) && (
        <div className="cdx-foot">
          {answerText && <CopyButton text={answerText} label={t("cdx_copy_response")} />}
          {!v.foldable && v.status === "worked" && <span className="x-meta tnum">{t("cdx_worked_for", { t: time })}</span>}
          {tokens != null && <span className="x-meta tnum">{t("hc_tokens", { n: tokens })}</span>}
        </div>
      ))}
    </section>
  );
});
