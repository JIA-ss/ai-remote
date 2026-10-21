// Settings > Network: which addresses are shown for this computer and what a person may type for
// a paired one. Uses the installed desktop TypeScript compiler, no extra dependency.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

function load() {
  const source = readFileSync(new URL("../apps/desktop/src/lib/network.ts", import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const exports = {};
  vm.runInNewContext(compiled, { exports });
  return exports;
}

const { shareableAddresses, validAddressInput } = load();

test("shows IPv4 first and hides addresses that only work on this computer", () => {
  const got = shareableAddresses([
    "[fe80::1c2:3:4:5]:47800",
    "127.0.0.1:47800",
    "[2001:db8::7]:47800",
    "192.168.1.20:47800",
    "169.254.10.2:47800",
    "192.168.1.20:47800",
  ]);
  assert.deepEqual([...got], ["192.168.1.20:47800", "[2001:db8::7]:47800"]);
});

test("takes IP addresses with or without a port, several at once", () => {
  for (const ok of ["192.168.1.20", "192.168.1.20:47800", "10.0.0.5, 10.0.0.6:50000", "[fe80::1]:9", "::1"]) {
    assert.equal(validAddressInput(ok), true, ok);
  }
});

test("refuses names, empty input and impossible numbers", () => {
  for (const bad of ["", "  , ", "office-pc", "192.168.1", "300.1.1.1", "10.0.0.5:0", "10.0.0.5:70000", "http://10.0.0.5"]) {
    assert.equal(validAddressInput(bad), false, bad);
  }
});
