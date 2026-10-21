// Syntax colors for code in the chat: a small tokenizer for the languages agents print most, enough
// to read code at a glance (keywords, strings, comments, numbers, function names). Not a parser: a
// wrong guess only colors a word wrongly. Tokens carry text only; rendering stays text nodes.
// Pure, tested in tools/chat-render.test.mjs.

export type TokenClass = "k" | "s" | "n" | "c" | "f" | "t" | "l" | "v" | "a" | "d" | "m" | "p";
export type Token = { c?: TokenClass; v: string };

interface Spec {
  keywords: string[];
  types?: string[];
  literals?: string[];
  line?: string[];
  block?: Array<[string, string]>;
  quotes?: string[];
  /** `$name` / `${name}` variables (shells). */
  vars?: boolean;
  /** Keys before `:` or `=` are names (JSON, YAML, TOML). */
  keys?: boolean;
}

const C_LIKE = ["if", "else", "for", "while", "do", "switch", "case", "default", "break", "continue", "return", "goto", "struct", "enum", "union", "typedef", "static", "const", "extern", "sizeof", "inline", "volatile"];
const JS = ["const", "let", "var", "function", "return", "if", "else", "for", "while", "do", "switch", "case", "default", "break", "continue", "new", "delete", "typeof", "instanceof", "in", "of", "class", "extends", "super", "this", "import", "export", "from", "as", "async", "await", "yield", "try", "catch", "finally", "throw", "void", "static", "get", "set"];
const TS_EXTRA = ["interface", "type", "enum", "implements", "namespace", "declare", "readonly", "public", "private", "protected", "abstract", "keyof", "satisfies", "is"];

const SPECS: Record<string, Spec> = {
  js: { keywords: JS, literals: ["true", "false", "null", "undefined", "NaN"], line: ["//"], block: [["/*", "*/"]], quotes: ["\"", "'", "`"] },
  ts: { keywords: [...JS, ...TS_EXTRA], types: ["string", "number", "boolean", "unknown", "any", "never", "object", "symbol", "bigint", "Record", "Promise", "Array"], literals: ["true", "false", "null", "undefined"], line: ["//"], block: [["/*", "*/"]], quotes: ["\"", "'", "`"] },
  json: { keywords: [], literals: ["true", "false", "null"], quotes: ["\""], keys: true },
  py: { keywords: ["def", "class", "return", "if", "elif", "else", "for", "while", "in", "not", "and", "or", "is", "import", "from", "as", "with", "try", "except", "finally", "raise", "pass", "break", "continue", "lambda", "yield", "global", "nonlocal", "assert", "del", "async", "await", "match", "case"], types: ["int", "str", "float", "bool", "list", "dict", "set", "tuple", "bytes", "object", "self", "cls"], literals: ["True", "False", "None"], line: ["#"], quotes: ["\"\"\"", "'''", "\"", "'"] },
  rs: { keywords: ["fn", "let", "mut", "const", "static", "if", "else", "match", "for", "while", "loop", "in", "return", "break", "continue", "struct", "enum", "impl", "trait", "type", "where", "pub", "crate", "mod", "use", "as", "self", "Self", "super", "move", "ref", "async", "await", "dyn", "unsafe", "extern"], types: ["i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool", "char", "str", "String", "Vec", "Option", "Result", "Box", "Some", "None", "Ok", "Err"], literals: ["true", "false"], line: ["//"], block: [["/*", "*/"]], quotes: ["\""] },
  go: { keywords: ["func", "var", "const", "type", "struct", "interface", "map", "chan", "if", "else", "for", "range", "switch", "case", "default", "return", "break", "continue", "go", "defer", "select", "package", "import", "fallthrough", "goto"], types: ["int", "int64", "int32", "uint", "uint64", "float64", "string", "bool", "byte", "rune", "error", "any"], literals: ["true", "false", "nil", "iota"], line: ["//"], block: [["/*", "*/"]], quotes: ["\"", "`", "'"] },
  c: { keywords: [...C_LIKE, "class", "public", "private", "protected", "virtual", "override", "template", "typename", "namespace", "using", "new", "delete", "this", "try", "catch", "throw", "auto", "constexpr", "nullptr", "import", "package", "extends", "implements", "interface", "final", "func", "var", "let", "fun", "val", "override", "readonly", "async", "await", "foreach", "is", "as"], types: ["int", "char", "float", "double", "long", "short", "unsigned", "signed", "void", "bool", "string", "String", "size_t", "byte", "object"], literals: ["true", "false", "null", "NULL", "nil"], line: ["//"], block: [["/*", "*/"]], quotes: ["\"", "'"] },
  sh: { keywords: ["if", "then", "else", "elif", "fi", "for", "in", "do", "done", "while", "until", "case", "esac", "function", "return", "exit", "local", "export", "set", "unset", "source", "echo", "cd", "sudo"], line: ["#"], quotes: ["\"", "'"], vars: true },
  ps: { keywords: ["if", "else", "elseif", "foreach", "for", "while", "do", "switch", "function", "param", "return", "try", "catch", "finally", "throw", "begin", "process", "end", "in"], literals: ["$true", "$false", "$null"], line: ["#"], block: [["<#", "#>"]], quotes: ["\"", "'"], vars: true },
  toml: { keywords: [], literals: ["true", "false"], line: ["#"], quotes: ["\"\"\"", "\"", "'"], keys: true },
  yaml: { keywords: [], literals: ["true", "false", "null", "yes", "no", "~"], line: ["#"], quotes: ["\"", "'"], keys: true },
  sql: { keywords: ["select", "from", "where", "and", "or", "not", "insert", "into", "values", "update", "set", "delete", "create", "table", "drop", "alter", "index", "join", "left", "right", "inner", "outer", "on", "group", "by", "order", "having", "limit", "as", "distinct", "union", "primary", "key", "null", "is", "in", "like", "case", "when", "then", "else", "end"], line: ["--"], block: [["/*", "*/"]], quotes: ["'", "\""] },
  css: { keywords: ["@media", "@import", "@keyframes", "!important", "@supports", "@layer"], line: [], block: [["/*", "*/"]], quotes: ["\"", "'"] },
  html: { keywords: [], block: [["<!--", "-->"]], quotes: ["\"", "'"] },
};

const ALIASES: Record<string, string> = {
  javascript: "js", jsx: "js", mjs: "js", cjs: "js", node: "js",
  typescript: "ts", tsx: "ts", mts: "ts",
  jsonc: "json", json5: "json",
  python: "py", python3: "py", py3: "py",
  rust: "rs",
  golang: "go",
  cpp: "c", "c++": "c", cc: "c", h: "c", hpp: "c", cs: "c", csharp: "c", java: "c", kotlin: "c", kt: "c", swift: "c", objc: "c", dart: "c", scala: "c",
  bash: "sh", zsh: "sh", shell: "sh", console: "sh", shellscript: "sh", fish: "sh",
  powershell: "ps", ps1: "ps", pwsh: "ps",
  yml: "yaml", ini: "toml", cfg: "toml", conf: "toml",
  postgres: "sql", mysql: "sql", sqlite: "sql",
  scss: "css", less: "css",
  xml: "html", svg: "html", vue: "html",
};

/** The language id a fence names, or null when it is not one we color. */
export function languageOf(lang: string | undefined): string | null {
  if (!lang) return null;
  const l = lang.toLowerCase();
  if (l === "diff" || l === "patch") return "diff";
  const id = ALIASES[l] ?? l;
  return SPECS[id] ? id : null;
}

/** Most text colored; longer code is shown plain (still copyable). */
const MAX = 60_000;

export function highlight(code: string, lang: string | undefined): Token[] {
  const id = languageOf(lang);
  if (!id || code.length > MAX) return [{ v: code }];
  if (id === "diff") return diffTokens(code);
  if (id === "html") return htmlTokens(code);
  return tokens(code, SPECS[id], id);
}

function diffTokens(code: string): Token[] {
  return code.split(/(?<=\n)/).map((line) => {
    if (/^(\+\+\+|---|@@|diff |index )/.test(line)) return { c: "m", v: line };
    if (line.startsWith("+")) return { c: "a", v: line };
    if (line.startsWith("-")) return { c: "d", v: line };
    return { v: line };
  });
}

function htmlTokens(code: string): Token[] {
  const out: Token[] = [];
  const re = /(<!--[\s\S]*?(?:-->|$))|(<\/?[\w:-]+)|("[^"]*"|'[^']*')|([\w:-]+)(?==)|(\/?>)/g;
  let at = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(code))) {
    if (m.index > at) out.push({ v: code.slice(at, m.index) });
    out.push({ c: m[1] ? "c" : m[2] ? "k" : m[3] ? "s" : m[4] ? "f" : "p", v: m[0] });
    at = re.lastIndex;
  }
  if (at < code.length) out.push({ v: code.slice(at) });
  return out;
}

const caseless = new Set(["sql", "ps"]);

function tokens(code: string, spec: Spec, id: string): Token[] {
  const fold = caseless.has(id);
  const kw = new Set(spec.keywords.map((k) => (fold ? k.toLowerCase() : k)));
  const types = new Set(spec.types ?? []);
  const lits = new Set((spec.literals ?? []).map((k) => (fold ? k.toLowerCase() : k)));
  const out: Token[] = [];
  let plain = "";
  const push = (c: TokenClass | undefined, v: string) => {
    if (!c) {
      plain += v;
      return;
    }
    if (plain) out.push({ v: plain });
    plain = "";
    out.push({ c, v });
  };
  let i = 0;
  let lineStart = true;
  while (i < code.length) {
    const ch = code[i];
    if (ch === "\n") {
      push(undefined, ch);
      i++;
      lineStart = true;
      continue;
    }
    const lc = spec.line?.find((m) => code.startsWith(m, i) && (m !== "#" || id !== "css"));
    if (lc) {
      const end = code.indexOf("\n", i);
      const stop = end < 0 ? code.length : end;
      push("c", code.slice(i, stop));
      i = stop;
      continue;
    }
    const bc = spec.block?.find(([open]) => code.startsWith(open, i));
    if (bc) {
      const end = code.indexOf(bc[1], i + bc[0].length);
      const stop = end < 0 ? code.length : end + bc[1].length;
      push("c", code.slice(i, stop));
      i = stop;
      continue;
    }
    const q = spec.quotes?.find((m) => code.startsWith(m, i));
    if (q) {
      let j = i + q.length;
      while (j < code.length && !code.startsWith(q, j)) {
        if (code[j] === "\\") j++;
        else if (code[j] === "\n" && q.length === 1 && q !== "`") break;
        j++;
      }
      const stop = Math.min(code.length, j + (code.startsWith(q, j) ? q.length : 0));
      const v = code.slice(i, stop);
      const key = spec.keys && /^\s*[:=]/.test(code.slice(stop, stop + 4)) && q.length === 1;
      push(key ? "f" : "s", v);
      i = stop;
      lineStart = false;
      continue;
    }
    if (spec.vars && ch === "$") {
      const m = /^\$(\{[^}\n]*\}|[A-Za-z_][\w:]*|[0-9?#@*$!-])/.exec(code.slice(i));
      if (m) {
        const lower = m[0].toLowerCase();
        push(lits.has(lower) ? "l" : "v", m[0]);
        i += m[0].length;
        continue;
      }
    }
    const num = /^(?:0[xX][\da-fA-F_]+|0[bB][01_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)[a-zA-Z]*\b/.exec(code.slice(i, i + 64));
    if (num && !/[\w$]/.test(code[i - 1] ?? "")) {
      push("n", num[0]);
      i += num[0].length;
      lineStart = false;
      continue;
    }
    const word = /^[@!]?[A-Za-z_$][\w$-]*/.exec(code.slice(i, i + 128));
    if (word) {
      let w = word[0];
      // A hyphen joins words only in CSS and shells (`--flag`, `font-size`).
      if (id !== "css" && id !== "sh" && id !== "ps") w = w.replace(/-.*$/, "");
      const key = fold ? w.toLowerCase() : w;
      const after = code.slice(i + w.length, i + w.length + 2);
      let c: TokenClass | undefined;
      // A word that runs on into a path or file name (`export.py`, `src/main`) is not a keyword.
      const pathy = /^[./\\][\w]/.test(code.slice(i + w.length, i + w.length + 2)) && /[./\\]/.test(code[i + w.length]);
      if (pathy && (id === "sh" || id === "ps")) c = lineStart ? "f" : undefined;
      else if (kw.has(key)) c = "k";
      else if (lits.has(key)) c = "l";
      else if (types.has(w)) c = "t";
      else if (spec.keys && lineStart && /^\s*[:=]/.test(code.slice(i + w.length, i + w.length + 3))) c = "f";
      else if (/^\s?\(/.test(after) || (id === "sh" && lineStart)) c = "f";
      else if (/^[A-Z][a-z]/.test(w) && id !== "sh" && id !== "sql") c = "t";
      push(c, w);
      i += w.length;
      lineStart = false;
      continue;
    }
    if (!/\s/.test(ch)) lineStart = id === "sh" && /[|;&]/.test(ch) ? true : false;
    push(undefined, ch);
    i++;
  }
  if (plain) out.push({ v: plain });
  return out;
}
