// Markdown from a coding agent, parsed into a small tree that the chat renders with React text
// nodes. Never HTML: raw tags in the text stay text, so nothing an agent writes can inject markup.
// The subset is what agents actually write (GitHub-flavored): headings, paragraphs, fenced code,
// block quotes, nested and task lists, tables, rules; inline code, emphasis, strikethrough and links.
// A reply that is still streaming parses too (an open fence is a code block until it closes).
// Pure, tested in tools/chat-render.test.mjs.

export type Inline =
  | { t: "text"; v: string }
  | { t: "code"; v: string }
  | { t: "em" | "strong" | "del"; c: Inline[] }
  | { t: "link"; href: string; c: Inline[] }
  | { t: "br" };

export type Align = "left" | "center" | "right" | null;

export type MdBlock =
  | { t: "p"; c: Inline[] }
  | { t: "h"; level: number; c: Inline[] }
  | { t: "code"; lang: string; text: string; open: boolean }
  | { t: "quote"; c: MdBlock[] }
  | { t: "list"; ordered: boolean; start: number; items: Array<{ task: boolean | null; c: MdBlock[] }> }
  | { t: "table"; align: Align[]; head: Inline[][]; rows: Inline[][][] }
  | { t: "hr" };

const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([^`\s]*)[^`]*$/;
const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/;
const HR = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const QUOTE = /^ {0,3}> ?(.*)$/;
const LIST = /^( {0,12})([-*+]|\d{1,9}[.)])([ \t]+|$)(.*)$/;
const TABLE_SEP = /^ {0,3}\|?[ \t]*:?-+:?[ \t]*(\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$/;

const blank = (l: string) => l.trim() === "";

function startsBlock(line: string, next: string | undefined): boolean {
  return FENCE.test(line) || HEADING.test(line) || HR.test(line) || QUOTE.test(line) || LIST.test(line) || isTableStart(line, next);
}

function isTableStart(line: string, next: string | undefined): boolean {
  return line.includes("|") && next !== undefined && TABLE_SEP.test(next) && next.includes("-");
}

/** The cells of a table row, without the outer pipes; `\|` stays a pipe inside a cell. */
function cells(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|") && !s.endsWith("\\|")) s = s.slice(0, -1);
  const out: string[] = [];
  let cur = "";
  let code = false;
  for (let i = 0; i < s.length; i++) {
    const ch = s[i];
    if (ch === "\\" && s[i + 1] === "|") {
      cur += "|";
      i++;
    } else if (ch === "`") {
      code = !code;
      cur += ch;
    } else if (ch === "|" && !code) {
      out.push(cur.trim());
      cur = "";
    } else {
      cur += ch;
    }
  }
  out.push(cur.trim());
  return out;
}

export function parseMarkdown(src: string): MdBlock[] {
  return parseLines(src.replace(/\r\n?/g, "\n").split("\n"));
}

function parseLines(lines: string[]): MdBlock[] {
  const out: MdBlock[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (blank(line)) {
      i++;
      continue;
    }
    let m: RegExpExecArray | null;
    if ((m = FENCE.exec(line))) {
      const mark = m[1];
      const indent = line.length - line.trimStart().length;
      const body: string[] = [];
      i++;
      let open = true;
      while (i < lines.length) {
        const l = lines[i];
        const close = /^ {0,3}(`{3,}|~{3,})[ \t]*$/.exec(l);
        if (close && close[1][0] === mark[0] && close[1].length >= mark.length) {
          open = false;
          i++;
          break;
        }
        body.push(indent ? l.replace(new RegExp(`^ {0,${indent}}`), "") : l);
        i++;
      }
      out.push({ t: "code", lang: m[2].toLowerCase(), text: body.join("\n"), open });
      continue;
    }
    if ((m = HEADING.exec(line))) {
      out.push({ t: "h", level: m[1].length, c: parseInline(m[2] ?? "") });
      i++;
      continue;
    }
    if (HR.test(line)) {
      out.push({ t: "hr" });
      i++;
      continue;
    }
    if (QUOTE.test(line)) {
      const body: string[] = [];
      while (i < lines.length && (m = QUOTE.exec(lines[i]))) {
        body.push(m[1]);
        i++;
      }
      out.push({ t: "quote", c: parseLines(body) });
      continue;
    }
    if (isTableStart(line, lines[i + 1])) {
      const head = cells(line);
      const align: Align[] = cells(lines[i + 1]).map((c) => (c.startsWith(":") && c.endsWith(":") ? "center" : c.endsWith(":") ? "right" : c.startsWith(":") ? "left" : null));
      i += 2;
      const rows: Inline[][][] = [];
      while (i < lines.length && !blank(lines[i]) && lines[i].includes("|")) {
        const r = cells(lines[i]);
        rows.push(head.map((_, k) => parseInline(r[k] ?? "")));
        i++;
      }
      out.push({ t: "table", align: head.map((_, k) => align[k] ?? null), head: head.map((h) => parseInline(h)), rows });
      continue;
    }
    if ((m = LIST.exec(line))) {
      const [list, next] = parseList(lines, i);
      out.push(list);
      i = next;
      continue;
    }
    // A paragraph: until a blank line or the start of another block.
    const para: string[] = [];
    while (i < lines.length && !blank(lines[i]) && (para.length === 0 || !startsBlock(lines[i], lines[i + 1]))) {
      para.push(lines[i].trim());
      i++;
    }
    out.push({ t: "p", c: parseInline(para.join("\n")) });
  }
  return out;
}

function parseList(lines: string[], from: number): [MdBlock, number] {
  const first = LIST.exec(lines[from]) as RegExpExecArray;
  const base = first[1].length;
  const ordered = /\d/.test(first[2]);
  const items: Array<{ task: boolean | null; c: MdBlock[] }> = [];
  let i = from;
  while (i < lines.length) {
    const m = LIST.exec(lines[i]);
    if (!m || m[1].length < base || m[1].length > base + 3 || /\d/.test(m[2]) !== ordered) break;
    // The item's text starts here; its further lines are those indented past the marker.
    const contentIndent = m[1].length + m[2].length + Math.max(1, Math.min(m[3].length, 4));
    const body: string[] = [m[4]];
    i++;
    while (i < lines.length) {
      const l = lines[i];
      if (blank(l)) {
        // A blank line continues the item only if what follows is indented into it.
        let k = i + 1;
        while (k < lines.length && blank(lines[k])) k++;
        if (k < lines.length && leading(lines[k]) >= contentIndent) {
          for (; i < k; i++) body.push("");
          continue;
        }
        break;
      }
      const lm = LIST.exec(l);
      if (leading(l) >= contentIndent) {
        body.push(l.slice(Math.min(contentIndent, leading(l))));
      } else if (lm && lm[1].length <= base + 3) {
        break; // the next item, or a list of another kind
      } else if (!startsBlock(l, lines[i + 1])) {
        body.push(l.trim()); // a lazy continuation of the item's paragraph
      } else {
        break;
      }
      i++;
    }
    let task: boolean | null = null;
    const tm = /^\[([ xX])\][ \t]+/.exec(body[0]);
    if (tm) {
      task = tm[1] !== " ";
      body[0] = body[0].slice(tm[0].length);
    }
    items.push({ task, c: parseLines(body) });
    let k = i;
    while (k < lines.length && blank(lines[k])) k++;
    const nm = k < lines.length ? LIST.exec(lines[k]) : null;
    if (k > i && nm && nm[1].length >= base && nm[1].length <= base + 3 && /\d/.test(nm[2]) === ordered) i = k;
  }
  return [{ t: "list", ordered, start: ordered ? parseInt(first[2], 10) : 1, items }, i];
}

function leading(l: string): number {
  let n = 0;
  for (const ch of l) {
    if (ch === " ") n++;
    else if (ch === "\t") n += 4 - (n % 4);
    else break;
  }
  return n;
}

const PUNCT = /[!-/:-@[-`{-~]/;
const URL_END = /[.,:;!?)\]'"]+$/;

/** Inline markdown of one block's text. */
export function parseInline(s: string): Inline[] {
  const out: Inline[] = [];
  let text = "";
  const flush = () => {
    if (text) out.push({ t: "text", v: text });
    text = "";
  };
  let i = 0;
  while (i < s.length) {
    const ch = s[i];
    if (ch === "\\" && i + 1 < s.length && PUNCT.test(s[i + 1])) {
      text += s[i + 1];
      i += 2;
      continue;
    }
    if (ch === "\n") {
      flush();
      out.push({ t: "br" });
      i++;
      continue;
    }
    if (ch === "`") {
      let n = 1;
      while (s[i + n] === "`") n++;
      const fence = "`".repeat(n);
      const end = s.indexOf(fence, i + n);
      if (end > 0 && s[end + n] !== "`") {
        flush();
        let v = s.slice(i + n, end).replace(/\n/g, " ");
        if (v.length > 2 && v.startsWith(" ") && v.endsWith(" ")) v = v.slice(1, -1);
        out.push({ t: "code", v });
        i = end + n;
        continue;
      }
      text += fence;
      i += n;
      continue;
    }
    if (ch === "[") {
      const close = matchBracket(s, i);
      if (close > 0 && s[close + 1] === "(") {
        const end = s.indexOf(")", close + 2);
        if (end > 0) {
          const href = s.slice(close + 2, end).trim().split(/\s+/)[0].replace(/^<|>$/g, "");
          flush();
          out.push({ t: "link", href, c: parseInline(s.slice(i + 1, close)) });
          i = end + 1;
          continue;
        }
      }
    }
    if (ch === "<") {
      const m = /^<((?:https?|mailto):[^\s<>]+)>/.exec(s.slice(i));
      if (m) {
        flush();
        out.push({ t: "link", href: m[1], c: [{ t: "text", v: m[1] }] });
        i += m[0].length;
        continue;
      }
    }
    if ((ch === "h" || ch === "w") && (i === 0 || /[\s(]/.test(s[i - 1]))) {
      const m = /^(?:https?:\/\/|www\.)[^\s<]+/.exec(s.slice(i));
      if (m) {
        const url = m[0].replace(URL_END, "");
        flush();
        out.push({ t: "link", href: url.startsWith("www.") ? `https://${url}` : url, c: [{ t: "text", v: url }] });
        i += url.length;
        continue;
      }
    }
    if (ch === "*" || ch === "_" || ch === "~") {
      let n = 1;
      while (s[i + n] === ch) n++;
      const run = Math.min(n, ch === "~" ? 2 : 3);
      if (ch === "~" && run < 2) {
        text += ch;
        i++;
        continue;
      }
      const prev = i > 0 ? s[i - 1] : " ";
      const after = s[i + run] ?? " ";
      // `_` inside a word (snake_case) is not emphasis; nor is a delimiter followed by a space.
      const opens = !/\s/.test(after) && !(ch === "_" && /\w/.test(prev));
      if (opens) {
        const close = findClose(s, i + run, ch, run);
        if (close > 0) {
          flush();
          const inner = parseInline(s.slice(i + run, close));
          out.push(ch === "~" ? { t: "del", c: inner } : run === 3 ? { t: "strong", c: [{ t: "em", c: inner }] } : { t: run === 2 ? "strong" : "em", c: inner });
          i = close + run;
          continue;
        }
      }
      text += s.slice(i, i + n);
      i += n;
      continue;
    }
    text += ch;
    i++;
  }
  flush();
  return out;
}

function matchBracket(s: string, at: number): number {
  let depth = 0;
  for (let i = at; i < s.length; i++) {
    if (s[i] === "\\") i++;
    else if (s[i] === "[") depth++;
    else if (s[i] === "]" && --depth === 0) return i;
    else if (s[i] === "\n" && s[i + 1] === "\n") return -1;
  }
  return -1;
}

function findClose(s: string, from: number, ch: string, run: number): number {
  const mark = ch.repeat(run);
  for (let i = from; i < s.length; i++) {
    if (s[i] === "\\") {
      i++;
      continue;
    }
    if (s[i] === "`") {
      const end = s.indexOf("`", i + 1);
      if (end > 0) i = end;
      continue;
    }
    if (s.startsWith(mark, i) && s[i + run] !== ch && s[i - 1] !== ch && i > from && !/\s/.test(s[i - 1])) {
      if (ch === "_" && /\w/.test(s[i + run] ?? "")) continue;
      return i;
    }
  }
  return -1;
}

/** The plain text of inline nodes (for titles and copying). */
export function inlineText(c: Inline[]): string {
  return c.map((x) => (x.t === "text" || x.t === "code" ? x.v : x.t === "br" ? "\n" : inlineText(x.c))).join("");
}

/** Whether a link may be shown as one: only web and mail addresses; anything else stays text. */
export function safeHref(href: string): string | null {
  return /^(https?:\/\/|mailto:)/i.test(href) ? href : null;
}
