// Claude tool output (TC-22/23): ANSI takes precedence; successful Bash output may otherwise
// highlight a unified or word diff. The host's text stays text, never HTML or executable links.

import { ansiOutput, type AnsiRun } from "./terminal";
export { ansiOutput } from "./terminal";

export interface OutputRun extends AnsiRun {
  change?: "add" | "del";
  line?: boolean;
}

const MAX_CHANGES = 2000;

function append(runs: OutputRun[], text: string, change?: OutputRun["change"], line?: boolean) {
  const last = runs[runs.length - 1];
  if (!change && last && !last.change) last.text += text;
  else if (text) runs.push({ text, ...(change ? { change, ...(line ? { line } : {}) } : {}) });
}

function wordDiff(text: string): OutputRun[] {
  const runs: OutputRun[] = [];
  const word = /\[-[\s\S]*?-\]|\{\+[\s\S]*?\+\}/g;
  let at = 0;
  let changes = 0;
  for (let m = word.exec(text); m && changes < MAX_CHANGES; m = word.exec(text)) {
    append(runs, text.slice(at, m.index));
    append(runs, m[0], m[0].startsWith("[-") ? "del" : "add");
    at = m.index + m[0].length;
    changes++;
  }
  append(runs, text.slice(at));
  return runs;
}

function lineDiff(text: string): { runs: OutputRun[]; valid: boolean } {
  const runs: OutputRun[] = [];
  let old = 0;
  let neu = 0;
  let headers = 0;
  let valid = true;
  let changes = 0;
  for (const line of text.match(/[^\n]*\n|[^\n]+$/g) ?? []) {
    const h = /^@@ -\d+(?:,(\d+))? \+\d+(?:,(\d+))? @@/.exec(line);
    let change: OutputRun["change"];
    if (h) {
      if (old || neu) valid = false;
      old = Number(h[1] ?? 1); neu = Number(h[2] ?? 1); headers++;
    } else if (!old && !neu && /^(?:--- |\+\+\+ )/.test(line)) {
      // File headers between completed hunks are metadata, rather than excess changed lines.
    } else if (headers && line.startsWith("+")) {
      if (neu > 0) { neu--; change = "add"; } else valid = false;
    } else if (headers && line.startsWith("-")) {
      if (old > 0) { old--; change = "del"; } else valid = false;
    } else if (headers && line.startsWith(" ")) {
      if (old > 0 && neu > 0) { old--; neu--; } else valid = false;
    } else if (!line.startsWith("\\ No newline at end of file")) {
      if (old || neu) valid = false;
      old = neu = 0;
    }
    if (change && changes++ >= MAX_CHANGES) change = undefined;
    append(runs, line, change, !!change);
  }
  return { runs, valid: headers > 0 && valid && old === 0 && neu === 0 };
}

/** Diff coloring is for successful shell output only; any ESC selects ANSI, even on an error. */
export function outputRuns(text: string, command = "", error = false): OutputRun[] {
  if (text.includes("\x1b")) return ansiOutput(text);
  if (/[\r\b]/.test(text)) text = ansiOutput(text).map((r) => r.text).join("");
  if (error) return [{ text }];
  if (/--(?:word-diff|color-words)\b/.test(command)) return wordDiff(text);
  const lines = lineDiff(text);
  return command || lines.valid ? lines.runs : wordDiff(text);
}
