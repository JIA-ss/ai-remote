// Words of the Claude view: a tool call as a verb and what it acted on, a group's sentence, durations
// and token counts. The rules are lib/claude.ts's (tested); this file only turns them into strings and
// spans (`cl_` strings, styles/claude.css).

import type { ReactNode } from "react";
import type { ToolItem } from "../../lib/chat";
import { durationParts, tenseOf, tokenCount, toolWords, type Cat, type Phrase, type Sentence, type ToolState } from "../../lib/claude";
import { currentLang, t, type Key } from "../../lib/i18n";
import { cx } from "../util";

const k = (s: string) => s as Key;

/** "1m 5s" / "1分5秒": at most three units from the largest that is not zero. */
export function dur(ms: number): string {
  return durationParts(ms).map((p) => `${p.n}${t(k(`cl_u_${p.unit}`))}`).join(t("cl_u_sep"));
}

/** "12.0k tokens". */
export function tokensLabel(n: number): string {
  return t(k(n === 1 ? "cl_tokens_one" : "cl_tokens_many"), { n: tokenCount(n) });
}

/** The string for a count: `<key>_one` for 1, `<key>_many` otherwise. */
export function countOf(key: string, n: number, vars: Record<string, string | number> = {}): string {
  return t(k(`${key}_${n === 1 ? "one" : "many"}`), { n, ...vars });
}

const upper = (s: string) => (s ? s[0].toUpperCase() + s.slice(1) : s);

/** One tool call as a phrase: "Read export.py", "Ran `git status`", "Running the test suite". */
export function ToolLabel({ x, st }: { x: ToolItem; st: ToolState }) {
  const tense = st === "denied" || st === "stopped" ? "failed" : tenseOf(st);
  const w = toolWords(x, tense);
  // A call that was said no to or stopped did not do what it set out to: its own words, not "Ran …".
  const held = (st === "denied" || st === "stopped") ? toolWords(x, "failed").failedText : undefined;
  let verb: string;
  if (held !== undefined) verb = upper(held);
  else if (w.verb) verb = t(k(`cl_t_${w.verb}_${tense}`), { n: w.n ?? 0 });
  else if (w.failedText !== undefined) verb = currentLang() === "en" ? t("cl_failed_to", { what: w.failedText }) : upper(w.failedText);
  else verb = w.text ?? "";
  let obj: ReactNode = null;
  if (w.obj) {
    obj = w.objKind === "code" ? <code className="cl-obj code">{w.obj}</code>
      : <span className={cx("cl-obj", w.objKind === "file" && "file")} title={w.path}>{w.obj}</span>;
  } else if (w.verb === "command") obj = <span className="cl-obj">{t("cl_obj_command")}</span>;
  return (
    <span className="cl-lbl">
      <span className="vb">{verb}</span>
      {obj}
    </span>
  );
}

const FILE_VERBS: Cat[] = ["read", "create", "edit", "delete"];

/** "(1 failed, 2 waiting)" after a phrase. */
function counts(p: { failed: number; blocked: number; stopped?: number; denied?: number }): string {
  const parts = [
    p.failed ? t("cl_g_n_failed", { n: p.failed }) : "",
    p.blocked ? t("cl_g_n_blocked", { n: p.blocked }) : "",
    p.denied ? t("cl_g_n_denied", { n: p.denied }) : "",
    p.stopped ? t("cl_g_n_stopped", { n: p.stopped }) : "",
  ].filter(Boolean);
  if (!parts.length) return "";
  return currentLang() === "zh" ? `（${parts.join("，")}）` : ` (${parts.join(", ")})`;
}

function phraseText(p: Phrase): string {
  return countOf(`cl_g_${p.cat}`, p.n) + counts(p);
}

/** A finished group summed up: "Read 2 files, ran 3 commands (1 failed)" or "Read and edited export.py". */
export function SentenceView({ s }: { s: Sentence }) {
  if (s.file) {
    const verbs = s.file.cats.filter((c) => FILE_VERBS.includes(c)).map((c) => t(k(`cl_gv_${c}`)));
    const joined = currentLang() === "zh" ? verbs.join("并") : new Intl.ListFormat("en", { type: "conjunction" }).format(verbs);
    const [before, after] = t("cl_g_file", { verbs: "\u0000", file: "\u0001" }).split("\u0000");
    const [mid, end] = (after ?? "").split("\u0001");
    const name = s.file.path.split(/[\\/]/).filter(Boolean).pop() ?? s.file.path;
    return (
      <span className="cl-sent">
        {upper(before + joined) + mid}
        <span className="cl-obj file" title={s.file.path}>{name}</span>
        {end}
      </span>
    );
  }
  const parts = s.phrases.map((p) => ({ text: phraseText(p), error: p.error }));
  if (s.more) parts.push({ text: countOf("cl_g_more", s.more.n) + counts(s.more), error: false });
  const sep = currentLang() === "zh" ? "，" : ", ";
  return (
    <span className="cl-sent">
      {parts.map((p, i) => (
        <span key={i} className={cx(p.error && "err")}>
          {i ? sep : ""}
          {i ? p.text : upper(p.text)}
        </span>
      ))}
    </span>
  );
}
