// What the chat panel renders from an agent's text (apps/desktop/src/lib/markdown.ts, highlight.ts)
// and the counted strings it uses, with the real modules transpiled. Markdown never produces
// markup: the tree holds text, and only web and mail links survive.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

function load(rel) {
  const source = readFileSync(new URL(`../apps/desktop/src/${rel}`, import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const exports = {};
  vm.runInNewContext(compiled, { exports, require: () => { throw new Error("no dependencies"); } }, { filename: rel });
  return exports;
}

const md = load("lib/markdown.ts");
const hl = load("lib/highlight.ts");
const { en } = load("lib/strings/en.ts");
const { zh } = load("lib/strings/zh.ts");
const plain = (v) => JSON.parse(JSON.stringify(v));
const kinds = (blocks) => blocks.map((b) => b.t);

test("block structure: headings, paragraphs, code, quotes, rules", () => {
  const b = md.parseMarkdown("# Title\n\nSome *text*\nnext line\n\n```rust\nfn main() {}\n```\n\n> quoted\n> more\n\n---\n");
  assert.deepEqual(plain(kinds(b)), ["h", "p", "code", "quote", "hr"]);
  assert.equal(b[0].level, 1);
  assert.deepEqual(plain(b[1].c.map((x) => x.t)), ["text", "em", "br", "text"]);
  assert.deepEqual(plain(b[2]), { t: "code", lang: "rust", text: "fn main() {}", open: false });
  assert.deepEqual(plain(kinds(b[3].c)), ["p"]);
});

test("a fence still streaming is a code block until it closes", () => {
  const b = md.parseMarkdown("Run:\n```sh\nnpm test\nnpm run");
  assert.deepEqual(plain(b[1]), { t: "code", lang: "sh", text: "npm test\nnpm run", open: true });
});

test("lists nest, number from their start and carry task boxes", () => {
  const b = md.parseMarkdown("3. first\n4. second\n   - inner a\n   - inner b\n\n- [x] done\n- [ ] open");
  assert.equal(b[0].t, "list");
  assert.equal(b[0].ordered, true);
  assert.equal(b[0].start, 3);
  assert.equal(b[0].items.length, 2);
  const inner = b[0].items[1].c.find((x) => x.t === "list");
  assert.equal(inner.items.length, 2);
  assert.equal(b[1].t, "list");
  assert.deepEqual(plain(b[1].items.map((i) => i.task)), [true, false]);
});

test("tables keep their alignment and escaped pipes", () => {
  const b = md.parseMarkdown("| Name | Size |\n|:-----|-----:|\n| `a\\|b` | 12 |\n| c | 3 |");
  assert.equal(b[0].t, "table");
  assert.deepEqual(plain(b[0].align), ["left", "right"]);
  assert.equal(b[0].rows.length, 2);
  assert.deepEqual(plain(b[0].rows[0][0]), [{ t: "code", v: "a|b" }]);
});

test("inline: code, strong, strikethrough, links; snake_case and raw HTML stay text", () => {
  const c = md.parseInline("**bold** `x_y` ~~gone~~ see [docs](https://example.com) or https://a.dev/x. some_snake_case <b>hi</b>");
  const types = plain(c.map((x) => x.t));
  assert.ok(types.includes("strong") && types.includes("code") && types.includes("del"));
  const links = c.filter((x) => x.t === "link").map((x) => x.href);
  assert.deepEqual(plain(links), ["https://example.com", "https://a.dev/x"]);
  const text = md.inlineText(c);
  assert.match(text, /some_snake_case <b>hi<\/b>$/);
  assert.equal(md.safeHref("javascript:alert(1)"), null);
  assert.equal(md.safeHref("file:///etc/passwd"), null);
  assert.equal(md.safeHref("https://ok.example"), "https://ok.example");
});

test("code is colored for known languages and left whole otherwise", () => {
  const toks = hl.highlight('const x = "hi"; // note\nfoo(1)', "ts");
  const by = (c) => toks.filter((t) => t.c === c).map((t) => t.v);
  assert.deepEqual(plain(by("k")), ["const"]);
  assert.deepEqual(plain(by("s")), ['"hi"']);
  assert.deepEqual(plain(by("c")), ["// note"]);
  assert.deepEqual(plain(by("f")), ["foo"]);
  assert.deepEqual(plain(by("n")), ["1"]);
  // Tokens put back together are the code, unchanged.
  assert.equal(toks.map((t) => t.v).join(""), 'const x = "hi"; // note\nfoo(1)');
  assert.deepEqual(plain(hl.highlight("anything", "brainfuck")), [{ v: "anything" }]);
  const sh = hl.highlight("echo $HOME # x", "bash");
  assert.ok(sh.some((t) => t.c === "v" && t.v === "$HOME"));
  const diff = hl.highlight("@@ -1 +1 @@\n-a\n+b\n", "diff");
  assert.deepEqual(plain(diff.map((t) => t.c ?? null)), ["m", "d", "a"]);
});

test("every counted string has a singular and a plural form, in both languages", () => {
  const counted = new Set();
  for (const k of Object.keys(en)) {
    const m = /^(.*)_(one|many)$/.exec(k);
    if (m && m[1].startsWith("hc_")) counted.add(m[1]);
  }
  assert.ok(counted.size >= 12, `found ${counted.size} counted strings`);
  for (const base of counted) {
    for (const form of ["one", "many"]) {
      assert.ok(en[`${base}_${form}`], `en ${base}_${form}`);
      assert.ok(zh[`${base}_${form}`], `zh ${base}_${form}`);
    }
    // English says "1 command" and "{n} commands": never "1 commands".
    assert.ok(!/\{n\}/.test(en[`${base}_one`]), `${base}_one names its 1`);
    assert.match(en[`${base}_many`], /\{n\}/);
  }
  assert.equal(Object.keys(en).length, Object.keys(zh).length, "both catalogs have the same keys");
});
