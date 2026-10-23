// A Codex turn, laid out the way Codex's own desktop app shows one (lib/codex.ts has the structure):
// the user's message on the right; "Working for …" over a hairline once the work starts; the agent's
// work as quiet one-line units that open in place — a command opens to its shell output, an edit to
// its diff with line numbers — with what the agent said between them; a line saying what it is at
// while nothing else does (its reasoning's title, or "Thinking"); the work folding behind "Worked for
// …" once the answer starts; the answer; and the files the turn changed, each opening to its diff.
// Questions are asked in a card in place of the composer (CodexRequestCard) and stay here as one
// "Asked" line once answered. The look was studied from Codex's app; the components are this
// project's own, on this app's Markdown and previews.

import { ChevronRight, CircleCheck, FileDiff, FilePen, FileText, Globe, MessageCircleQuestion, Minimize2, Play, Plug, Terminal } from "lucide-react";
import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { baseName, type QuestionBlock, type ToolItem, type Turn } from "../../lib/chat";
import {
  activeAct,
  codexTurn,
  durationA,
  durationB,
  groupSummary,
  filePatch,
  formatMsgTime,
  parseAnsi,
  webDetail,
  relPath,
  workedMs,
  type Act,
  type CmdAction,
  type DiffLine,
  type ExecInfo,
  type FileChange,
  type Unit,
} from "../../lib/codex";
import { currentLang, t, type Key } from "../../lib/i18n";
import { cx } from "../util";
import { CodeBlock, CopyButton, Highlighted, Markdown } from "../chat/Markdown";
import { PreviewButton, TurnFiles, usePreview } from "../chat/Preview";
import { previewKind } from "../../lib/preview";
import { NoticeRow, UserRow } from "../chat/Rows";

const zh = () => currentLang() === "zh";
/** The language durations are written in. */
export const lang = () => (zh() ? "zh-CN" : "en");
const plural = (key: string, n: number, vars: Record<string, string | number> = {}) => t(`${key}_${n === 1 ? "one" : "many"}` as Key, { n, ...vars });
const toolRunning = (x: ToolItem) => x.status === "in_progress" || x.status === "pending";

/** Text that shimmers all the time while it is on (a sub-agent at work, a secondary label). */
function Shimmer({ children, on }: { children: ReactNode; on: boolean }) {
  return <span className={cx(on && "cdx-shimmer")}>{children}</span>;
}

const reducedMotion = () => typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

/**
 * Text the agent is at now: it rests in the dimmer color and, 600 ms after it shows and then every
 * 4 s, a band of light sweeps over it once (Codex's cadenced shimmer; none with reduced motion).
 */
export function Cadenced({ children, on }: { children: ReactNode; on: boolean }) {
  // Which sweep this is (a new one starts afresh even if the last never said it ended), and whether it runs.
  const [n, setN] = useState(0);
  const [sweep, setSweep] = useState(false);
  useEffect(() => {
    if (!on || reducedMotion()) return;
    let timer = setTimeout(function next() {
      setN((k) => k + 1);
      setSweep(true);
      timer = setTimeout(next, 4000);
    }, 600);
    return () => clearTimeout(timer);
  }, [on]);
  if (!on) return <>{children}</>;
  return (
    <span className="cdx-cad" data-sweep={sweep ? "" : undefined}>
      <span className="x-base">{children}</span>
      {/* The bright copy exists only while it sweeps (so find and copy see the text once). */}
      {sweep && (
        <span key={n} className="x-sweep" aria-hidden onAnimationEnd={(e) => e.target === e.currentTarget && setSweep(false)}>
          <span className="x-hi">{children}</span>
        </span>
      )}
    </span>
  );
}

function Chevron({ open }: { open: boolean }) {
  return <ChevronRight size={12} className={cx("cdx-chev", open && "open")} aria-hidden />;
}

/** Opens and closes its body with an animation (Codex's activity disclosure); what is open when it
 * first shows (a transcript being loaded) appears without one. */
function Reveal({ open, children, className, quiet }: { open: boolean; children: ReactNode; className?: string;
  /** Changes not made by the reader (the turn's answer arrived) happen at once, without an animation. */
  quiet?: boolean }) {
  const [shown, setShown] = useState(open);
  const [moved, setMoved] = useState(false);
  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    if (!quiet) setMoved(true);
    if (open) setShown(true);
    else if (quiet) setShown(false);
    else {
      const timer = setTimeout(() => setShown(false), 160);
      return () => clearTimeout(timer);
    }
  }, [open]);
  if (!open && (!shown || quiet)) return null;
  return <div className={cx("cdx-reveal", open ? moved && "x-in" : "x-out", className)}>{children}</div>;
}

/** A one-line activity: its words, and (when it has any) details that open below it. */
function ActLine({ label, live, steady, failed, details, defaultOpen = false, className, trailing }: {
  label: ReactNode;
  live?: boolean;
  /** Live, it shimmers all the time (a secondary label, as a sub-agent at work) instead of now and then. */
  steady?: boolean;
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
          <span className="x-tx">{steady ? <Shimmer on={!!live}>{label}</Shimmer> : <Cadenced on={!!live}>{label}</Cadenced>}</span>
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
  const took = done ? durationA(info.durationMs, lang()) : null;
  return (
    <>
      <Verb k={!done ? "cdx_v_running" : x.stopped ? "cdx_v_stopped" : "cdx_v_ran"} /> <code className="x-cmd">{oneLine(info.command)}</code>
      {took && <Detail> {t(x.stopped ? "cdx_after_time" : "cdx_in_time", { t: took })}</Detail>}
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
      <Verb k={failed ? "cdx_v_failed_edit" : fileVerb(f, done)} />{" "}
      <Detail>
        <span className="x-path" title={relPath(f.path, cwd)}>{baseName(f.path)}</span>
        {f.movePath && <> {t("cdx_d_to", { p: baseName(f.movePath) })}</>}
      </Detail>{" "}
      {!failed && <Stat add={f.add} del={f.del} />}
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
          {a.query && <Detail>: {webDetail(a.query)}</Detail>}
        </>
      );
    case "tool":
      return (
        <>
          <Verb k={toolRunning(a.tool) ? "cdx_v_calling" : a.tool.status === "failed" ? "cdx_v_failed" : "cdx_v_called"} /> <Detail><code className="x-cmd">{toolName(a.tool)}</code></Detail>
        </>
      );
  }
}

// ---------------------------------------------------------------------------------------------
// Details: a command's shell, an edit's diff, a tool's input and output

/**
 * A command's output: its end in view (the box keeps its bottom pinned as output arrives, with no
 * script: a reversed column), the rest a scroll away, long lines scrolling sideways; edges fade where
 * there is more to scroll to.
 */
function OutputBox({ text }: { text: string }) {
  const box = useRef<HTMLDivElement>(null);
  const [edges, setEdges] = useState({ up: false, down: false });
  const measure = () => {
    const el = box.current;
    if (!el) return;
    // In a reversed column the bottom is scrollTop 0 and going up is negative.
    const fromBottom = Math.abs(el.scrollTop);
    const room = el.scrollHeight - el.clientHeight;
    const next = { up: room - fromBottom > 1, down: fromBottom > 1 };
    setEdges((e) => (e.up === next.up && e.down === next.down ? e : next));
  };
  useLayoutEffect(measure, [text]);
  const runs = useMemo(() => parseAnsi(text), [text]);
  return (
    <div ref={box} className="x-out-box" data-up={edges.up || undefined} data-down={edges.down || undefined} onScroll={measure} tabIndex={0}>
      <pre className="x-out">
        {runs.length === 1 && Object.keys(runs[0]).length === 1 ? runs[0].text : runs.map((r, i) => (
          <span key={i} className={cx(r.fg?.startsWith("c") && `x-f${r.fg.slice(1)}`, r.bg?.startsWith("c") && `x-b${r.bg.slice(1)}`, r.bold && "x-bo",
            r.dim && "x-dim", r.italic && "x-it", r.underline && "x-un", r.strike && "x-st", r.hidden && "x-hid")}
            style={r.fg?.startsWith("rgb") || r.bg?.startsWith("rgb") ? { color: r.fg?.startsWith("rgb") ? r.fg : undefined, background: r.bg?.startsWith("rgb") ? r.bg : undefined } : undefined}>
            {r.text}
          </span>
        ))}
      </pre>
    </div>
  );
}

/** A command, two lines at most until clicked. */
function CommandLine({ command }: { command: string }) {
  const [all, setAll] = useState(false);
  const long = command.split("\n").length > 2 || command.length > 160;
  return (
    <pre className={cx("x-cmd", long && !all && "x-clamp")} onClick={() => long && setAll((v) => !v)} title={long && !all ? t("cdx_show_all") : undefined}>
      <span className="x-ps">$ </span><Highlighted code={command} lang="sh" />
    </pre>
  );
}

function ShellCard({ info, x }: { info: ExecInfo; x: ToolItem }) {
  const out = info.output.replace(/\n+$/, "");
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
        <CommandLine command={info.command} />
        {out ? <OutputBox text={out} /> : !running && <div className="x-none">{t("cdx_no_output")}</div>}
        {x.truncated && <div className="x-none">{t("hc_cut")}</div>}
      </div>
      {/* How it ended, in the same quiet gray whatever it was; nothing yet while it runs. */}
      <div className="x-sh-ft">
        {running ? "\u00a0"
          : x.stopped ? t("cdx_stopped")
            : info.exitCode === 0 ? <><CircleCheck size={10} aria-hidden /> {t("cdx_success")}</>
              : info.exitCode !== null ? t("cdx_exit_code", { n: info.exitCode })
                : t("cdx_exit_unknown")}
      </div>
    </div>
  );
}

/** Most diff lines drawn at once; the rest open on request. */
const DIFF_LINES = 400;

export function DiffView({ lines }: { lines: DiffLine[] }) {
  const [all, setAll] = useState(false);
  const shown = all ? lines : lines.slice(0, DIFF_LINES);
  // One width for every line's numbers, so added and removed lines start where the others do.
  const digits = useMemo(() => String(lines.reduce((m, l) => (l.kind === "hunk" ? m : Math.max(m, l.old ?? 0, l.new ?? 0)), 0)).length, [lines]);
  if (!lines.length) return <div className="cdx-diff x-empty">{t("cdx_too_large")}</div>;
  return (
    <div className="cdx-diff" tabIndex={0} style={{ "--x-nw": `${Math.max(digits, 2)}ch` } as React.CSSProperties}>
      {shown.map((l, i) =>
        l.kind === "hunk" ? (
          // A hunk's header: the code it is in (as git names it), or a gap between two hunks.
          i === 0 && !l.text.replace(/^@@[^@]*@@\s?/, "") ? null : (
            <div key={i} className="x-hunk" role="separator">{l.text.replace(/^@@[^@]*@@\s?/, "") || "⋯"}</div>
          )
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
      // A web search is one line that does not open; one without a query says nothing.
      if (!a.query.trim() && !toolRunning(a.tool)) return null;
      return <ActLine label={actWords(a, cwd)} live={toolRunning(a.tool)} />;
    case "tool":
      return <ActLine label={actWords(a, cwd)} live={toolRunning(a.tool)} failed={a.tool.status === "failed" && !a.tool.stopped}
        details={hasToolDetails(a.tool) ? () => <ToolDetails x={a.tool} /> : null} />;
  }
}

function FileButtons({ f, cwd, x }: { f: FileChange; cwd: string | null; x: ToolItem }) {
  if (x.status === "failed" || toolRunning(x)) return null;
  const patch = filePatch(f, cwd);
  return (
    <span className="x-btns">
      {f.kind !== "delete" && <PreviewButton path={f.movePath ?? f.path} />}
      {patch && <CopyButton text={patch} label={t("cdx_copy_patch")} />}
    </span>
  );
}

// ---------------------------------------------------------------------------------------------
// Units

const PART_ICON = { tools: Plug, edited: FilePen, failed: FilePen, read: FileText, ran: Terminal, web: Globe } as const;

/** A group's line once done: an icon for what it did first, then all it did as one list ("Edited files, read files, ran a command"). */
function summarySentence(acts: Act[]): ReactNode {
  const s = groupSummary(acts);
  if (!s.parts.length) return t("cdx_s_done");
  const words = s.parts.map((p, i) => plural(`cdx_s_${p.part}${i === 0 ? "_lead" : ""}`, p.n));
  let text = words.join(zh() ? "，" : ", ");
  if (!zh()) {
    try {
      text = new Intl.ListFormat("en", { type: "unit" }).format(words);
    } catch {
      // An engine without ListFormat keeps the plain list.
    }
  }
  const Icon = PART_ICON[s.parts[0].part];
  return (
    <>
      <Icon size={14} className="x-ic" aria-hidden />
      {text}
    </>
  );
}

/** A list that fades at an edge where there is more to scroll to. */
function ScrollList({ children }: { children: ReactNode }) {
  const box = useRef<HTMLDivElement>(null);
  const [edges, setEdges] = useState({ up: false, down: false });
  const measure = () => {
    const el = box.current;
    if (!el) return;
    const next = { up: el.scrollTop > 1, down: el.scrollHeight - el.clientHeight - el.scrollTop > 1 };
    setEdges((e) => (e.up === next.up && e.down === next.down ? e : next));
  };
  useLayoutEffect(measure);
  return (
    <div ref={box} className="cdx-list" data-up={edges.up || undefined} data-down={edges.down || undefined} onScroll={measure}>
      {children}
    </div>
  );
}

/**
 * A live label that holds for at least `ms` before the next one replaces it, so a quick run of calls
 * does not flicker past; once not live (the summary) it changes at once.
 */
function useDwell(key: string, node: ReactNode, live: boolean, ms = 1000): ReactNode {
  const [shown, setShown] = useState(() => ({ key, node, at: Date.now() }));
  const latest = useRef({ key, node });
  latest.current = { key, node };
  useEffect(() => {
    if (!live || key === shown.key) return;
    const wait = shown.at + ms - Date.now();
    if (wait <= 0) {
      setShown({ key, node, at: Date.now() });
      return;
    }
    const timer = setTimeout(() => setShown({ ...latest.current, at: Date.now() }), wait);
    return () => clearTimeout(timer);
  }, [key, live, shown]);
  return !live || key === shown.key ? node : shown.node;
}

/** Tool calls in a row: one line naming what runs now, or what was done; opens to each call. */
const GroupRow = memo(function GroupRow({ u, cwd, title }: { u: Extract<Unit, { kind: "group" }>; cwd: string | null;
  /** The title of the agent's latest reasoning, said while no call runs. */
  title: string | null }) {
  const acts = u.acts;
  const single = acts.length === 1 && !(acts[0].kind === "patch" && acts[0].files.length > 1);
  // Live: the call running now; between calls, the reading the agent is in the middle of, its
  // reasoning's title, or "Thinking".
  const active = u.live ? activeAct(acts) : null;
  const doing = active && (toolRunning(active.tool) || (active.kind === "exec" && active.info.explore)) ? active : null;
  const liveKey = doing ? `a:${doing.key}:${toolRunning(doing.tool)}` : title ? `t:${title}` : "thinking";
  const liveLabel = doing ? actWords(doing, cwd) : title ?? t("cdx_thinking");
  const label = useDwell(liveKey, liveLabel, u.live && !single);
  // A single call is its own line, live or done (the line does not change component while it runs).
  if (single) return <ActRow a={acts[0]} cwd={cwd} />;
  const failed = acts.some((a) => a.tool.status === "failed" && !a.tool.stopped);
  return (
    <ActLine className="group" label={u.live ? label : summarySentence(acts)} live={u.live} failed={!u.live && failed}
      details={() => (
        <ScrollList>
          {u.acts.map((a) => <ActRow key={a.key} a={a} cwd={cwd} />)}
        </ScrollList>
      )} />
  );
});

function AgentRow({ x }: { x: ToolItem }) {
  const out = x.content.filter((c): c is { type: "text"; text: string } => c.type === "text").map((c) => c.text).join("\n").trim();
  const prompt = (x.input as { prompt?: unknown } | undefined)?.prompt;
  return (
    <ActLine label={<><Verb k="cdx_subagent" /> <Detail>{x.title.replace(/^agent\s*/i, "")}</Detail></>} live={toolRunning(x)} steady failed={x.status === "failed"}
      details={out || typeof prompt === "string" ? () => (
        <div className="cdx-tool">
          {typeof prompt === "string" && <Markdown text={prompt} className="cdx-thought-md" />}
          {out && <Markdown text={out} />}
        </div>
      ) : null} />
  );
}

/** Questions the agent asked (one request), once answered or closed: "Asked 2 questions", opening to each with its answer. */
function AskedRow({ questions }: { questions: QuestionBlock[] }) {
  return (
    <ActLine className="asked" label={<><Verb k="cdx_q_asked" /> <Detail>{plural("cdx_q_n", questions.length)}</Detail></>}
      details={() => (
        <div className="cdx-asked">
          {questions.map((q) => {
            const given = q.answers ?? (q.answer ? [q.answer] : []);
            return (
              <div key={q.qid} className="x-q">
                <div className="x-qt">{q.prompt}</div>
                <div className="x-qa">{given.length ? (q.secret ? "••••••" : given.join(", ")) : t("cdx_q_no_answer")}</div>
              </div>
            );
          })}
        </div>
      )} />
  );
}

function UnitView({ u, cwd, title }: { u: Unit; cwd: string | null; title: string | null }): ReactNode {
  switch (u.kind) {
    case "group":
      return <GroupRow u={u} cwd={cwd} title={title} />;
    case "message":
      return <Markdown text={u.block.text} className={cx("cdx-md", u.streaming && "streaming")} />;
    case "agent":
      return <AgentRow x={u.tool} />;
    case "asked":
      return <AskedRow questions={u.questions} />;
    case "notice":
      if (u.code === "compacted") return <div className="cdx-compact"><Minimize2 size={14} aria-hidden /><span>{t("cdx_compacted")}</span></div>;
      return <NoticeRow level={u.level} code={u.code} text={u.text} />;
    case "steer":
      return <UserRow row={{ kind: "user", key: u.key, text: u.block.text, by: u.block.by, ts: u.block.ts, steer: true, inputId: u.block.inputId, attachments: u.block.attachments }} />;
  }
}

// ---------------------------------------------------------------------------------------------
// The changes a turn made

/** Files listed before "Show N more files". */
const FILES_SHOWN = 3;
/** A diff longer than this is not drawn inline. */
const INLINE_LINES = 5000;

/**
 * The files the turn changed, as Codex sums them up under its answer: "Edited 3 files" with their
 * counts beside a diff icon, Review opening every diff at once; with several files a short list of
 * them (three, then the rest on request), each opening to its own diff.
 */
function ChangesCard({ files, cwd }: { files: FileChange[]; cwd: string | null }) {
  const [more, setMore] = useState(false);
  const [review, setReview] = useState(false);
  const patches = useMemo(() => files.map((f) => filePatch(f, cwd)).filter((p): p is string => p !== null), [files, cwd]);
  const add = files.reduce((n, f) => n + f.add, 0);
  const del = files.reduce((n, f) => n + f.del, 0);
  const single = files.length === 1 ? files[0] : null;
  const shown = more ? files : files.slice(0, FILES_SHOWN);
  const title = single ? t("cdx_edited_file", { name: baseName(single.movePath ?? single.path) }) : t("cdx_edited_files", { n: files.length });
  return (
    <div className={cx("cdx-changes", review && "x-review")}>
      <div className="x-hd">
        <button type="button" className="x-hd-ln" onClick={() => setReview((v) => !v)} aria-expanded={review}>
          <span className="x-sq" aria-hidden><FileDiff size={24} /></span>
          <span className="x-hd-tx">
            <span className="x-ti">{title}</span>
            <span className="x-sub"><Stat add={add} del={del} /></span>
            <span className="x-sub x-alt">{t("cdx_review_changes")} ›</span>
          </span>
        </button>
        <span className="x-acts">
          {patches.length > 0 && <CopyButton text={patches.join("")} label={t("cdx_copy_patch")} />}
          <button type="button" className="btn sm x-outline" onClick={() => setReview((v) => !v)}>{t("cdx_review")}</button>
        </span>
      </div>
      {single ? (
        <Reveal open={review}><div className="x-one"><DiffView lines={single.lines} /></div></Reveal>
      ) : (
        <div className="x-files">
          {shown.map((f) => <ChangedFile key={f.path} f={f} cwd={cwd} all={review} />)}
          {files.length > FILES_SHOWN && (
            <button type="button" className="x-more" onClick={() => setMore((v) => !v)} aria-expanded={more}>
              <span>{more ? t("cdx_collapse_files") : plural("cdx_show_more_files", files.length - FILES_SHOWN)}</span>
              <ChevronRight size={12} className={cx("cdx-chev", "x-on", more && "x-up")} aria-hidden />
            </button>
          )}
        </div>
      )}
    </div>
  );
}

function ChangedFile({ f, cwd, all }: { f: FileChange; cwd: string | null; all: boolean }) {
  // Opened or closed by hand, until Review opens or closes them all.
  const [own, setOwn] = useState<boolean | null>(null);
  useEffect(() => setOwn(null), [all]);
  const preview = usePreview();
  const target = f.movePath ?? f.path;
  const viewable = !!preview && f.kind !== "delete" && !!previewKind(target);
  const big = f.lines.length > INLINE_LINES;
  const can = f.lines.length > 0 && !big;
  const open = can && (own ?? all);
  const rel = relPath(f.movePath ?? f.path, cwd);
  return (
    <div className={cx("x-file", open && "open")}>
      <div className="x-row">
        <button type="button" className="x-ln" aria-expanded={can ? open : undefined} disabled={!can && !viewable}
          onClick={(e) => {
            // ⌘ / Ctrl + click opens the file; a click opens its diff.
            if ((e.metaKey || e.ctrlKey || !can) && viewable) preview!.open(target);
            else if (can) setOwn(!open);
          }}>
          <span className="x-path" title={rel}>
            <span className="x-dir">{rel.slice(0, rel.length - baseName(rel).length)}</span>
            <span className="x-nm">{baseName(rel)}</span>
          </span>
          {big && <span className="x-big">{t("cdx_too_large_inline")}</span>}
          <Stat add={f.add} del={f.del} />
          {f.kind === "add" && <i className="x-dot x-new" title={t("cdx_v_created")} />}
          {f.kind === "delete" && <i className="x-dot x-gone" title={t("cdx_v_deleted")} />}
        </button>
        {f.kind !== "delete" && <PreviewButton path={target} />}
      </div>
      <Reveal open={open}><DiffView lines={f.lines} /></Reveal>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// The turn

/** Folded turns, by session and turn, so a turn opened once stays open while the panel lives. */
const opened = new Map<string, boolean>();

export const CodexTurnView = memo(function CodexTurnView({ turn, session, now, readonly, stopped, stopping = false, cwd, far, onContinue }: {
  turn: Turn;
  session: string;
  now: number;
  readonly: boolean;
  /** The last turn, stopped: it offers to continue. */
  stopped: boolean;
  /** Stop was pressed and the turn has not ended yet. */
  stopping?: boolean;
  cwd: string | null;
  far: boolean;
  onContinue: () => void;
}) {
  const v = useMemo(() => codexTurn(turn, cwd), [turn, cwd]);
  const key = `${session}:${turn.n}`;
  const [open, setOpen] = useState<boolean>(() => opened.get(key) ?? false);
  const folded = v.foldable && !open;
  const running = turn.status === "running";
  const ms = workedMs(turn, now);
  const time = durationB(ms, lang());
  const answerText = v.answer?.block.text ?? "";
  const work = v.units;
  // The work folds by itself once the answer starts (at once); the reader's own clicks animate it.
  const byReader = useRef(false);
  // A turn that starts while it is shown lets its "Working" line come in; a replayed one does not.
  const appear = useRef(running);
  const toggle = () => {
    byReader.current = true;
    opened.set(key, !open);
    setOpen(!open);
  };
  // "Working for 12s" over a hairline once the work starts, held still once the answer starts ("Worked
  // for"); a stopped or failed turn says after how long.
  const timer = stopping ? t("hc_st_stopping")
    : v.status === "stopped" ? t("cdx_stopped_after", { t: time })
      : v.status === "failed" ? t("cdx_failed_after", { t: time })
        : v.status === "worked" || v.answer ? t("cdx_worked_for", { t: time })
          : ms >= 1000 ? t("cdx_working_for", { t: time }) : t("cdx_working");
  const divider = work.length > 0 || v.status === "stopped";
  return (
    <section className={cx("cdx-turn", running && "running", far && "far")} data-turn={turn.n}>
      {turn.user && (
        <UserRow row={{ kind: "user", key: `u${turn.n}`, text: turn.user.text, by: turn.user.by, ts: turn.user.ts, steer: false, inputId: turn.user.inputId, attachments: turn.user.attachments }} />
      )}
      {divider && (
        <div className={cx("cdx-worked", appear.current && "x-new", v.status === "stopped" && "x-stop", v.status === "failed" && "x-fail")}>
          {v.foldable ? (
            <button type="button" className="x-tg" onClick={toggle} aria-expanded={open}>
              <span className="tnum">{timer}</span>
              <Chevron open={open} />
            </button>
          ) : (
            <span className="x-tg tnum">{timer}</span>
          )}
          <div className="x-rule" />
        </div>
      )}
      {/* What the user added to the running turn stays in view when the work folds (as in Codex). */}
      {folded && work.filter((u) => u.kind === "steer").map((u) => (
        <div key={u.key} className="cdx-unit" data-unit={u.kind}>
          <UnitView u={u} cwd={cwd} title={v.title} />
        </div>
      ))}
      {work.length > 0 && (
        <Reveal open={!folded} className="cdx-work" quiet={!byReader.current}>
          {work.map((u) => (
            <div key={u.key} className="cdx-unit" data-unit={u.kind}>
              <UnitView u={u} cwd={cwd} title={v.title} />
            </div>
          ))}
        </Reveal>
      )}
      {running && (v.asking ? (
        <div className="cdx-thinking x-ask"><MessageCircleQuestion size={14} aria-hidden /><span>{t("cdx_waiting_answer")}</span></div>
      ) : stopping ? (
        !divider && <div className="cdx-thinking">{t("hc_st_stopping")}</div>
      ) : v.thinking && (
        <div className="cdx-thinking"><Cadenced on>{v.title ?? t("cdx_thinking")}</Cadenced></div>
      ))}
      {v.answer && <Markdown text={answerText} className={cx("cdx-md cdx-answer", v.answer.streaming && "streaming")} />}
      {v.status === "failed" && (
        <NoticeRow level="error" code="failed" text={turn.reason ? `${t("hc_failed")}: ${turn.reason}` : t("hc_failed")} />
      )}
      {v.changes.length > 0 && <ChangesCard files={v.changes} cwd={cwd} />}
      {!running && <TurnFiles turn={turn} />}
      {!running && (stopped && !readonly ? (
        <div className="nt-stopped">
          <span>{turn.reason ? `${t("hc_stopped_lede")} ${turn.reason}` : t("hc_stopped_lede")}</span>
          <button type="button" className="btn sm" onClick={onContinue}>
            <Play size={12} aria-hidden />
            {t("hc_continue")}
          </button>
        </div>
      ) : answerText && (
        <div className="cdx-foot">
          <CopyButton text={answerText} label={t("cdx_copy_response")} />
          {v.answer?.block.startedTs !== undefined && (
            <span className="x-meta tnum" title={new Date(v.answer.block.startedTs).toLocaleString()}>{formatMsgTime(v.answer.block.startedTs, Date.now(), lang())}</span>
          )}
        </div>
      ))}
    </section>
  );
});
