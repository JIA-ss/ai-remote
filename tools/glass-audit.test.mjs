// The glass audit's static lint (tools/desktop-ui-smoke.py --lint): what Firefox-based Camofox cannot
// see at runtime, a literal -webkit-backdrop-filter before every backdrop-filter and the
// data-material gate on every blurring rule. Fixture sheets only, so the result does not depend on
// the state of the real style sheets.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const GATE = ':root:is([data-material="native"], [data-material="preview"])';

function lint(css) {
  const dir = mkdtempSync(join(tmpdir(), "glass-audit-"));
  try {
    const file = join(dir, "fixture.css");
    writeFileSync(file, css);
    const run = spawnSync(process.platform === "win32" ? "python" : "python3",
      [fileURLToPath(new URL("./desktop-ui-smoke.py", import.meta.url)), "--lint", file], { encoding: "utf8" });
    assert.equal(run.error, undefined, String(run.error));
    return { status: run.status, out: run.stdout + run.stderr };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test("a gated rule with the literal prefixed declaration first passes", () => {
  const { status, out } = lint(`/* a brace in a comment { */
${GATE} .a { color: red; -webkit-backdrop-filter: blur(4px) saturate(1.8); backdrop-filter: var(--gl-blur); }
${GATE}[data-os="windows"] :is(.b, .c) { -webkit-backdrop-filter: blur(2px); backdrop-filter: blur(2px); }
.d { -webkit-backdrop-filter: none; backdrop-filter: none; }`);
  assert.equal(status, 0, out);
  assert.match(out, /^4 checks passed$/m);
});

test("a missing or var() prefixed declaration fails", () => {
  const { status, out } = lint(`${GATE} .a { backdrop-filter: blur(4px); }
${GATE} .b { -webkit-backdrop-filter: var(--x); backdrop-filter: var(--x); }
${GATE} .c { backdrop-filter: blur(1px); -webkit-backdrop-filter: blur(1px); }`);
  assert.equal(status, 1, out);
  assert.match(out, /FAIL lint prefixed literal -webkit-backdrop-filter first: .*\.a/);
  assert.match(out, /FAIL lint prefixed literal -webkit-backdrop-filter first: .*\.b/);
  assert.match(out, /FAIL lint prefixed literal -webkit-backdrop-filter first: .*\.c/);
  assert.match(out, /^3 checks passed, 3 failed$/m);
});

test("every selector of a blurring rule must be gated, also inside at-rules", () => {
  const { status, out } = lint(`.plain { -webkit-backdrop-filter: blur(1px); backdrop-filter: blur(1px); }
@media (min-width: 1px) { @supports (display: grid) {
  ${GATE} .ok, .leak:is(.x, .y) { -webkit-backdrop-filter: blur(1px); backdrop-filter: blur(1px); }
} }`);
  assert.equal(status, 1, out);
  assert.match(out, /FAIL lint gated on data-material: .*\.plain .*"ungated": \["\.plain"\]/);
  assert.match(out, /"ungated": \["\.leak:is\(\.x, \.y\)"\]/);
  assert.match(out, /^2 checks passed, 2 failed$/m);
});
