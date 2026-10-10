import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, writeFileSync, rmSync, copyFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";
import { mcpInstall, shellArgument } from "../apps/desktop/src/lib/mcp-install.ts";

const launch = {
  command: "/Applications/O'Brien $HOME `printf bad`/ai-remote.app/Contents/MacOS/ai-remote",
  args: ["--home", "/Users/测试/Profile's state; $(printf bad)", "mcp"],
};

test("Claude and Codex shell commands preserve every literal argument", { skip: process.platform === "win32" }, () => {
  const dir = mkdtempSync(join(tmpdir(), "ar-mcp-"));
  try {
    for (const client of ["claude", "codex"]) {
      writeFileSync(join(dir, client), '#!/usr/bin/env python3\nimport sys,json\nprint(json.dumps(sys.argv[1:]))\n', { mode: 0o755 });
      const command = mcpInstall(client, launch, false).command;
      const args = JSON.parse(execFileSync("/bin/sh", ["-c", command], { encoding: "utf8", env: { ...process.env, PATH: `${dir}:${process.env.PATH}` } }));
      assert.deepEqual(args.slice(args.indexOf("--") + 1), [launch.command, ...launch.args]);
      assert.ok(args.includes("ai-remote"));
      if (client === "claude") assert.equal(args[args.indexOf("--scope") + 1], "user");
    }
  } finally { rmSync(dir, { recursive: true }); }
});

test("client JSON uses its own schema without changing executable or profile", () => {
  for (const client of ["cursor", "other"]) {
    assert.deepEqual(JSON.parse(mcpInstall(client, launch, false).config).mcpServers["ai-remote"], launch);
  }
  assert.deepEqual(JSON.parse(mcpInstall("vscode", launch, false).config).servers["ai-remote"], { type: "stdio", ...launch });
});

test("Codex TOML round trips paths with spaces, Unicode and backslashes", () => {
  for (const config of [launch, { command: "C:\\Program Files\\ai-remote\\ai-remote.exe", args: ["--home", "C:\\Users\\O'Brien\\配置", "mcp"] }]) {
    const text = mcpInstall("codex", config, true).config;
    const parsed = JSON.parse(execFileSync("python3", ["-c", "import sys,json,tomllib; print(json.dumps(tomllib.loads(sys.stdin.read())))"], { input: text, encoding: "utf8" }));
    assert.deepEqual(parsed.mcp_servers["ai-remote"], config);
  }
});

test("PowerShell arguments escape apostrophes and leave backslashes literal", () => {
  assert.equal(shellArgument("C:\\Users\\O'Brien\\ai-remote.exe", true), "'C:\\Users\\O''Brien\\ai-remote.exe'");
});

test("PowerShell commands preserve each native client argument", { skip: process.platform !== "win32" }, () => {
  const launch = { command: "C:\\Program Files\\O'Brien $HOME\\ai-remote.exe", args: ["--home", "C:\\Users\\测试\\Profile's state; $(Write-Output bad)", "mcp"] };
  const dir = mkdtempSync(join(tmpdir(), "ar-mcp-"));
  try {
    const preload = join(dir, "argv.cjs");
    writeFileSync(preload, "console.log(JSON.stringify(process.argv.slice(1))); process.exit(0);");
    for (const client of ["claude", "codex"]) {
      copyFileSync(process.execPath, join(dir, `${client}.exe`));
      const args = JSON.parse(execFileSync("pwsh", ["-NoProfile", "-Command", mcpInstall(client, launch, true).command], {
        encoding: "utf8", env: { ...process.env, PATH: `${dir};${process.env.PATH}`, NODE_OPTIONS: `--require "${preload}"` },
      }));
      assert.deepEqual(args.slice(args.indexOf("--") + 1), [launch.command, ...launch.args]);
    }
  } finally { rmSync(dir, { recursive: true }); }
});
