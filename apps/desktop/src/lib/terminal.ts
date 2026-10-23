// Terminal text as safe styled runs: SGR colors and attributes, with CR/backspace overwrites.
// Renderers opt into this helper explicitly; nothing here interprets HTML or navigates links.

export interface AnsiRun {
  text: string;
  fg?: string;
  bg?: string;
  bold?: boolean;
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
  strike?: boolean;
}

const PALETTE = [
  "#4d4d4c", "#c82829", "#718c00", "#eab700", "#4271ae", "#8959a8", "#3e999f", "#d6d6d6",
  "#8e908c", "#f5871f", "#a2bd40", "#f0c674", "#6ca4e0", "#b777db", "#6bc8cd", "#ffffff",
];

function color256(n: number): string | undefined {
  if (!Number.isInteger(n) || n < 0 || n > 255) return undefined;
  if (n < 16) return PALETTE[n];
  if (n >= 232) { const g = 8 + (n - 232) * 10; return `rgb(${g} ${g} ${g})`; }
  const k = n - 16;
  const v = (i: number) => i === 0 ? 0 : 55 + i * 40;
  return `rgb(${v(Math.floor(k / 36))} ${v(Math.floor(k / 6) % 6)} ${v(k % 6)})`;
}

/** ANSI SGR colors and attributes, including inverse video. All other escapes are discarded. */
export function ansiOutput(text: string): AnsiRun[] {
  const runs: AnsiRun[] = [];
  let style: Omit<AnsiRun, "text"> = {};
  let inverse = false;
  const push = (s: string) => {
    if (!s) return;
    const st = inverse ? { ...style, fg: style.bg ?? "Canvas", bg: style.fg ?? "CanvasText" } : style;
    const last = runs[runs.length - 1];
    if (last && JSON.stringify({ ...last, text: "" }) === JSON.stringify({ ...st, text: "" })) last.text += s;
    else runs.push({ text: s, ...st });
  };
  const escape = /\x1b(?:\[([0-9;]*)m|\[[0-?]*[ -/]*[@-~]|\[[0-?]*[ -/]*$|\][\s\S]*?(?:\x07|\x1b\\|$)|[PX^_][\s\S]*?(?:\x1b\\|$)|[()][0-~]|.)|\x1b/g;
  let at = 0;
  for (let m = escape.exec(text); m; m = escape.exec(text)) {
    push(text.slice(at, m.index));
    at = m.index + m[0].length;
    if (m[1] === undefined) continue;
    const codes = m[1].split(";").map((n) => Number(n || 0));
    for (let i = 0; i < codes.length; i++) {
      const c = codes[i];
      if (c === 0) { style = {}; inverse = false; }
      else if (c === 1) style.bold = true;
      else if (c === 2) style.dim = true;
      else if (c === 3) style.italic = true;
      else if (c === 4) style.underline = true;
      else if (c === 7) inverse = true;
      else if (c === 9) style.strike = true;
      else if (c === 22) { delete style.bold; delete style.dim; }
      else if (c === 23) delete style.italic;
      else if (c === 24) delete style.underline;
      else if (c === 27) inverse = false;
      else if (c === 29) delete style.strike;
      else if (c === 39) delete style.fg;
      else if (c === 49) delete style.bg;
      else if (c >= 30 && c <= 37) style.fg = PALETTE[c - 30];
      else if (c >= 90 && c <= 97) style.fg = PALETTE[c - 90 + 8];
      else if (c >= 40 && c <= 47) style.bg = PALETTE[c - 40];
      else if (c >= 100 && c <= 107) style.bg = PALETTE[c - 100 + 8];
      else if ((c === 38 || c === 48) && (codes[i + 1] === 5 || codes[i + 1] === 2)) {
        const channel = c === 38 ? "fg" : "bg";
        if (codes[i + 1] === 5) {
          const color = color256(codes[i + 2]);
          if (color) style[channel] = color;
          i += 2;
        } else {
          const rgb = codes.slice(i + 2, i + 5);
          if (rgb.length === 3) style[channel] = `rgb(${rgb.map((v) => Math.max(0, Math.min(255, v))).join(" ")})`;
          i += 4;
        }
      }
    }
  }
  push(text.slice(at));
  return /[\r\b]/.test(runs.map((r) => r.text).join("")) ? rewriteLines(runs) : runs;
}

/** Cursor edits operate on styled characters, so overwriting text also overwrites its color. */
function rewriteLines(runs: AnsiRun[]): AnsiRun[] {
  const result: AnsiRun[] = [];
  let cells: Array<{ char: string; style: AnsiRun }> = [];
  let cursor = 0;
  let lastStyle: AnsiRun | undefined;
  const append = (char: string, style: AnsiRun) => {
    if (lastStyle === style) result[result.length - 1].text += char;
    else { result.push({ ...style, text: char }); lastStyle = style; }
  };
  const flush = () => {
    for (const cell of cells) append(cell.char, cell.style);
    cells = []; cursor = 0;
  };
  for (const run of runs) {
    for (const char of run.text) {
      if (char === "\r") cursor = 0;
      else if (char === "\b") cursor = Math.max(0, cursor - 1);
      else if (char === "\n") { flush(); append("\n", run); }
      else { cells[cursor] = { char, style: run }; cursor++; }
    }
  }
  flush();
  return result;
}
