// The shipped style sheet keeps the literal -webkit-backdrop-filter values the glass layer writes.
// Vite's production minifier (Lightning CSS) folds a -webkit- / unprefixed declaration pair into one
// value taken from the unprefixed declaration; older WebKit only reads the prefixed form and ignores
// var() in it, so a var() in the unprefixed declaration would silently turn the glass off there.
// This builds the app's front end into a temporary directory (the real `vite build`, not a model of
// it) and reads the CSS it produced. Skipped when the desktop app's node_modules are not installed.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const desktop = fileURLToPath(new URL("../apps/desktop/", import.meta.url));
const vite = join(desktop, "node_modules", "vite", "bin", "vite.js");

test("the production CSS keeps literal prefixed blur values", { skip: !existsSync(vite) && "apps/desktop/node_modules is not installed" }, () => {
  const out = mkdtempSync(join(tmpdir(), "glass-build-"));
  try {
    const run = spawnSync(process.execPath, [vite, "build", "--outDir", out, "--emptyOutDir", "--logLevel", "error"], {
      cwd: desktop,
      encoding: "utf8",
    });
    assert.equal(run.status, 0, run.stdout + run.stderr);
    const assets = join(out, "assets");
    const css = readdirSync(assets).filter((f) => f.endsWith(".css")).map((f) => readFileSync(join(assets, f), "utf8")).join("\n");
    assert.ok(css.length > 0, "no CSS was emitted");
    // declarations only (after { or ;), not the @supports conditions
    const prefixed = [...css.matchAll(/[{;]-webkit-backdrop-filter:([^;}]*)/g)].map((m) => m[1]);
    // regular (24 px), thick (36 px) and Windows (30 px) glass, each as a literal
    for (const want of ["blur(24px)saturate(180%)", "blur(36px)saturate(160%)", "blur(30px)saturate(125%)"])
      assert.ok(prefixed.some((v) => v.replace(/\s+/g, "") === want), `no -webkit-backdrop-filter:${want} in ${JSON.stringify(prefixed)}`);
    const withVar = prefixed.filter((v) => v.includes("var("));
    assert.deepEqual(withVar, [], "a prefixed declaration in the built CSS reads a custom property");
  } finally {
    rmSync(out, { recursive: true, force: true });
  }
});
