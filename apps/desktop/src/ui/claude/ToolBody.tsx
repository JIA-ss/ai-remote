// What an opened tool row shows (TC-18 … TC-34): a command and its output, a file as read, an edit as
// a diff with the file's real line numbers when the host sent hunks, the task list, a plan, or the
// call's input and output for any other tool. Output boxes stop at 400 px and scroll.

import { Check, Circle, CircleCheck, LoaderCircle } from "lucide-react";
import { memo, useMemo, useState } from "react";
import { baseName, toolCommand, toolOutput, type ToolContent, type ToolItem } from "../../lib/chat";
import { bodyKind, contentStat, diffLines, inputLines, inputOf, readText, toolName, type ToolState } from "../../lib/claude";
import { ansiOutput, outputRuns, type OutputRun } from "../../lib/claude-output";
import { t } from "../../lib/i18n";
import { cx } from "../util";
import { CopyButton, Markdown } from "../chat/Markdown";
import { PreviewButton } from "../chat/Preview";
import { ansiStyle, renderAnsiText } from "../chat/Ansi";
import { dur } from "./text";

type Diff = Extract<ToolContent, { type: "diff" }>;

/** Lines shown before "Show all": a long output starts with its last lines. */
const TAIL = 40;

/** A tool's output, scrolling past 400 px; long ones show their last lines until asked. */
function Output({ text, error, command }: { text: string; error?: boolean; command?: string }) {
  const [all, setAll] = useState(false);
  const body = text.replace(/\n+$/, "");
  // Parse before choosing the tail: an ANSI style can start before the last 40 lines.
  const runs = useMemo<OutputRun[]>(() => command === undefined ? ansiOutput(body) : outputRuns(body, command, error), [body, command, error]);
  const lines = runs.map((r) => r.text).join("").split("\n");
  const cut = !all && lines.length > TAIL;
  const skip = cut ? lines.slice(0, -TAIL).join("\n").length + 1 : 0;
  let at = 0;
  return (
    <div className="cl-out-box">
      {cut && (
        <button type="button" className="cl-more" onClick={() => setAll(true)}>
          {t("cl_out_all", { n: lines.length })}
        </button>
      )}
      <pre className={cx("cl-out", error && "err")} tabIndex={0} role="group" aria-label={t("cl_out_label")}>
        {runs.map((r, i) => {
          const start = at;
          at += r.text.length;
          if (at <= skip) return null;
          return <span key={i} className={cx(r.change && `cl-out-${r.change}`, r.line && "cl-out-line")}
            style={ansiStyle(r)}>
            {r.text.slice(Math.max(0, skip - start))}
          </span>;
        })}
      </pre>
    </div>
  );
}

/** Why a call shows no output: the person said no, or the turn was stopped (TC-21). */
function Settled({ st }: { st: ToolState }) {
  if (st === "denied") return <div className="cl-settled">{t("cl_out_denied")}</div>;
  if (st === "stopped") return <div className="cl-settled">{t("cl_out_stopped")}</div>;
  return null;
}

function BashBody({ x, st, now }: { x: ToolItem; st: ToolState; now: number }) {
  const cmd = toolCommand(x) ?? (inputOf(x).command as string | undefined) ?? "";
  const out = toolOutput(x);
  const ps = toolName(x) === "PowerShell" ? "> " : "$ ";
  const elapsed = st === "busy" ? now - x.startedTs : 0;
  return (
    <div className="cl-bash">
      {cmd && (
        <div className="cl-cmd">
          <code>
            <span className="ps" aria-hidden>{ps}</span>
            {cmd}
          </code>
          <CopyButton text={`${ps}${cmd}${out ? `\n\n${out}` : ""}`} className="cl-copy" />
        </div>
      )}
      {st === "denied" || st === "stopped" ? <Settled st={st} /> : out && <Output text={out} error={st === "failed"} command={cmd} />}
      {st === "busy" && elapsed >= 10_000 && (
        <div className="cl-run-ft">
          <span>{t("cl_bash_running")}</span>
          <span className="tnum">{dur(elapsed)}</span>
        </div>
      )}
    </div>
  );
}

function ReadBody({ x, st }: { x: ToolItem; st: ToolState }) {
  const out = toolOutput(x);
  if (st !== "done") return st === "failed" && out ? <Output text={out} error /> : <Settled st={st} />;
  const { text, first } = readText(out);
  if (!text) return <div className="cl-settled">{t("cl_out_empty")}</div>;
  const lines = text.split("\n");
  return (
    <div className="cl-read" tabIndex={0} role="group" aria-label={t("cl_out_label")}>
      {lines.map((l, i) => (
        <div key={i} className="ln">
          {first !== null && <span className="no tnum">{first + i}</span>}
          <span className="tx">{l}</span>
        </div>
      ))}
    </div>
  );
}

/** One file's change: a header with the name and counts, then the lines (numbered from hunks). */
export const DiffView = memo(function DiffView({ c, head = true, preview = false }: { c: Diff; head?: boolean; preview?: boolean }) {
  const { rows, numbered } = diffLines(c);
  const st = contentStat(c);
  const created = c.old === null || c.old === "";
  return (
    <div className="cl-diff">
      {head && (
        <div className="cl-diff-hd" title={c.path}>
          <span className="fn">{baseName(c.path)}</span>
          {created && <span className="new">{t("cl_diff_new")}</span>}
          <span className="stat tnum">
            <span className="add">+{st.add}</span>
            <span className="del">−{st.del}</span>
          </span>
          {preview && <PreviewButton path={c.path} />}
        </div>
      )}
      <div className={cx("cl-diff-rows", numbered && "numbered")} tabIndex={0}>
        {rows.map((r, i) =>
          r.kind === "gap" ? (
            <div key={i} className="gap">{r.lines ? t("cl_diff_gap", { n: r.lines }) : t("cl_diff_gap_some")}</div>
          ) : (
            <div key={i} className={cx("ln", r.kind)}>
              {numbered && <span className="no tnum">{r.old ?? ""}</span>}
              {numbered && <span className="no tnum">{r.new ?? ""}</span>}
              <span className="sg" aria-hidden>{r.kind === "add" ? "+" : r.kind === "del" ? "−" : " "}</span>
              <span className="tx">{r.text}</span>
            </div>
          ),
        )}
      </div>
    </div>
  );
});

function TodoBody({ x }: { x: ToolItem }) {
  const todos = (inputOf(x).todos as Array<{ content?: string; status?: string; activeForm?: string }> | undefined) ?? [];
  return (
    <ul className="cl-todos" role="list">
      {todos.map((td, i) => (
        <li key={i} className={td.status}>
          {td.status === "completed" ? <CircleCheck size={14} aria-hidden /> : td.status === "in_progress" ? <LoaderCircle size={14} className="spin" aria-hidden /> : <Circle size={14} aria-hidden />}
          <span>{td.content}</span>
        </li>
      ))}
    </ul>
  );
}

function PlanBody({ x, st }: { x: ToolItem; st: ToolState }) {
  const plan = String(inputOf(x).plan ?? "");
  return (
    <div className="cl-planbody">
      {st === "done" && (
        <div className="ok">
          <Check size={14} aria-hidden />
          {t("cl_plan_ok")}
        </div>
      )}
      {plan && <Markdown text={plan} className="cl-plan-md" renderAnsi={renderAnsiText} />}
    </div>
  );
}

function GenericBody({ x, st }: { x: ToolItem; st: ToolState }) {
  const lines = inputLines(x.input);
  const out = toolOutput(x);
  return (
    <div className="cl-generic">
      {lines.length > 0 && (
        <dl className="cl-kv">
          {lines.map((l) => (
            <div key={l.key}>
              <dt>{l.key}</dt>
              <dd className={cx(l.mono && "mono")}>{l.value}</dd>
            </div>
          ))}
        </dl>
      )}
      {st === "denied" || st === "stopped" ? <Settled st={st} /> : out && <Output text={out} error={st === "failed"} />}
    </div>
  );
}

/** The body of an opened tool row. */
export function ToolBody({ x, st, now, previewedPath }: { x: ToolItem; st: ToolState; now: number; previewedPath?: string | null }) {
  const kind = bodyKind(x);
  return (
    <div className="cl-tool-bd">
      {kind === "bash" && <BashBody x={x} st={st} now={now} />}
      {kind === "read" && <ReadBody x={x} st={st} />}
      {kind === "diff" && x.content.map((c, i) => (c.type === "diff" ? <DiffView key={i} c={c}
        preview={x.status !== "failed" && c.path !== previewedPath && !(c.new === "" && c.old)} /> : null))}
      {kind === "todo" && <TodoBody x={x} />}
      {kind === "plan" && <PlanBody x={x} st={st} />}
      {kind === "generic" && <GenericBody x={x} st={st} />}
      {x.truncated && <div className="cl-settled">{t("cl_out_cut")}</div>}
    </div>
  );
}
