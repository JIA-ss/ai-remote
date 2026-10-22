// File previews in the chat panel (apps/desktop/src/lib/preview.ts), with the real module
// transpiled: which paths can be previewed, which text in a reply names a file, and that an HTML
// preview's document carries its network-blocking policy in front of the page's own markup.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

function load(rel) {
  const source = readFileSync(new URL(`../apps/desktop/src/${rel}`, import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const exports = {};
  vm.runInNewContext(compiled, { exports, TextDecoder, require: () => { throw new Error("no dependencies"); } }, { filename: rel });
  return exports;
}

const pv = load("lib/preview.ts");
const { en } = load("lib/strings/en.ts");
const { zh } = load("lib/strings/zh.ts");

test("kinds come from the extension, case-insensitively", () => {
  assert.equal(pv.previewKind("/tmp/out/Report.HTML"), "html");
  assert.equal(pv.previewKind("C:\\work\\notes.md"), "markdown");
  assert.equal(pv.previewKind("chart.svg"), "image");
  assert.equal(pv.previewKind("a.txt"), null);
  assert.equal(pv.previewKind(".md"), null);
  assert.equal(pv.previewKind("Makefile"), null);
  assert.equal(pv.imageMime("x.SVG"), "image/svg+xml");
  assert.equal(pv.imageMime("x.jpg"), "image/jpeg");
});

test("files named in a reply: relative, absolute, Windows, file: links, with anchors and line numbers", () => {
  assert.equal(pv.fileTarget("out/report.html"), "out/report.html");
  assert.equal(pv.fileTarget("./notes.md"), "./notes.md");
  assert.equal(pv.fileTarget("C:\\work\\chart.png"), "C:\\work\\chart.png");
  assert.equal(pv.fileTarget("C:/work/chart.png"), "C:/work/chart.png");
  assert.equal(pv.fileTarget("file:///tmp/a%20b.svg"), "/tmp/a b.svg");
  assert.equal(pv.fileTarget("file:///C:/x/r.html"), "C:/x/r.html");
  assert.equal(pv.fileTarget("report.html#top"), "report.html");
  assert.equal(pv.fileTarget("docs/guide.md:12"), "docs/guide.md");
  // Not files, or not ones a preview shows.
  assert.equal(pv.fileTarget("https://example.com/a.html"), null);
  assert.equal(pv.fileTarget("mailto:a@b.md"), null);
  assert.equal(pv.fileTarget("javascript:alert(1)//.html"), null);
  assert.equal(pv.fileTarget("npm run build"), null);
  assert.equal(pv.fileTarget("src/main.rs"), null);
  assert.equal(pv.fileTarget("out/"), null);
});

test("an HTML preview's document puts the policy before the page's markup", () => {
  const guard = /<meta http-equiv="Content-Security-Policy" content="default-src 'none';[^"]*base-uri 'none'; form-action 'none'"><base target="_blank">/;
  const withHead = pv.previewDocument("<!doctype html><html><head><title>x</title><script>1</script></head><body>hi</body></html>");
  assert.match(withHead, /^<!doctype html><html><head><meta http-equiv/);
  assert.ok(withHead.indexOf("Content-Security-Policy") < withHead.indexOf("<script>"));
  assert.match(withHead, guard);
  const noHead = pv.previewDocument("<!DOCTYPE html>\n<p>hi</p>");
  assert.match(noHead, /^<!DOCTYPE html><meta http-equiv/);
  assert.match(pv.previewDocument("<p>hi</p>"), /^<meta http-equiv/);
  // A policy of its own can only narrow ours: it comes after.
  const own = pv.previewDocument('<head><meta http-equiv="Content-Security-Policy" content="default-src *">');
  assert.ok(own.indexOf("default-src 'none'") < own.indexOf("default-src *"));
});

test("text is cut on a character boundary", () => {
  const bytes = new TextEncoder().encode("ab中文");
  const whole = pv.decodeText(bytes);
  assert.equal(whole.text, "ab中文");
  assert.equal(whole.cut, false);
  const cut = pv.decodeText(bytes, 4);
  assert.equal(cut.cut, true);
  assert.equal(cut.text, "ab");
  assert.equal(pv.sizeLabel(512), "512 B");
  assert.equal(pv.sizeLabel(12 * 1024), "12 KB");
  assert.equal(pv.sizeLabel(3.4 * 1048576), "3.4 MB");
});

test("the preview strings exist in both languages", () => {
  const keys = Object.keys(en).filter((k) => k.startsWith("hc_preview_"));
  assert.ok(keys.length >= 10);
  for (const k of keys) assert.equal(typeof zh[k], "string", k);
  assert.match(en.hc_preview_open, /\{name\}/);
  assert.match(zh.hc_preview_open, /\{name\}/);
});

test("a turn's files: what its tools wrote, then what its reply names, one per name", () => {
  const tool = (o) => ({ kind: "tool", callId: "c", title: "", tool: "edit", status: "completed", locations: [], content: [], truncated: false, startedTs: 0, endedTs: 0, ...o });
  const blocks = [
    tool({ content: [{ type: "diff", path: "/w/out/report.html", old: null, new: "<p>" }] }),
    tool({ tool: "read", locations: ["/w/in/source.md"] }),
    tool({ status: "failed", content: [{ type: "diff", path: "/w/broken.md", old: null, new: "x" }] }),
    tool({ content: [{ type: "diff", path: "/w/gone.png", old: "x", new: "" }] }),
    tool({ tool: "execute", locations: ["/w/out/chart.svg", "/w/main.rs"] }),
    { kind: "text", id: "m", text: "Wrote [the page](out/report.html), `out/notes.md` and `README.txt`; see https://x.dev/a.md" },
  ];
  const files = pv.turnFiles(blocks);
  assert.equal(JSON.stringify(files), JSON.stringify(["/w/out/report.html", "/w/out/chart.svg", "out/notes.md"]));
  const many = Array.from({ length: 6 }, (_, i) => tool({ content: [{ type: "diff", path: `/w/p${i}.md`, old: null, new: "x" }] }));
  assert.equal(pv.turnFiles(many).length, pv.MAX_INLINE);
});
