// The start form's folder picker (apps/desktop/src/lib/chat.ts, the folder helpers): host paths may be
// Windows or Unix, and `~` is the host user's home. The real module is transpiled.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

function load() {
  const source = readFileSync(new URL("../apps/desktop/src/lib/chat.ts", import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const exports = {};
  const store = new Map();
  const localStorage = { getItem: (k) => store.get(k) ?? null, setItem: (k, v) => store.set(k, String(v)), removeItem: (k) => store.delete(k) };
  vm.runInNewContext(compiled, { exports, require: () => { throw new Error("no dependencies"); }, localStorage, JSON, Set, Map }, { filename: "chat.ts" });
  return exports;
}

const c = load();
const plain = (v) => JSON.parse(JSON.stringify(v));

test("what is a path and what is a search", () => {
  for (const p of ["/", "/home/a", "~", "~/code", "~\\code", "C:", "c:\\", "D:\\work\\x", "D:/work", "\\\\srv\\share"]) assert.ok(c.looksLikePath(p), p);
  for (const p of ["app", "code/app", "~code", ""]) assert.ok(!c.looksLikePath(p), p);
});

test("trailing separators go, roots stay", () => {
  assert.equal(c.trimFolder("/home/a/"), "/home/a");
  assert.equal(c.trimFolder("/"), "/");
  assert.equal(c.trimFolder("D:\\work\\"), "D:\\work");
  assert.equal(c.trimFolder("D:\\"), "D:\\");
  assert.equal(c.trimFolder("D:"), "D:\\");
  assert.equal(c.trimFolder("D:/"), "D:/");
  assert.equal(c.trimFolder(" ~/code/ "), "~/code");
});

test("names, parents, children and crumbs on both kinds of path", () => {
  assert.equal(c.folderName("D:\\work\\app"), "app");
  assert.equal(c.folderName("/Users/a/code/app/"), "app");
  assert.equal(c.folderName("C:\\"), "C:");
  assert.equal(c.folderName("/"), "/");
  assert.equal(c.folderName("~"), "~");
  assert.equal(c.folderName("\\\\srv\\share"), "share");
  assert.equal(c.parentFolder("D:\\work\\app"), "D:\\work");
  assert.equal(c.parentFolder("D:\\work"), "D:\\");
  assert.equal(c.parentFolder("D:\\"), null);
  assert.equal(c.parentFolder("/a"), "/");
  assert.equal(c.parentFolder("~/code"), "~");
  assert.equal(c.parentFolder("\\\\srv\\share\\x"), "\\\\srv\\share\\");
  assert.equal(c.childFolder("D:\\", "work"), "D:\\work");
  assert.equal(c.childFolder("D:\\work", "app"), "D:\\work\\app");
  assert.equal(c.childFolder("/", "tmp"), "/tmp");
  assert.equal(c.childFolder("~", "code"), "~/code");
  assert.equal(c.childFolder("C:/Users", "a"), "C:/Users/a");
  assert.deepEqual(plain(c.folderCrumbs("D:\\work\\app")), [
    { name: "D:", path: "D:\\" }, { name: "work", path: "D:\\work" }, { name: "app", path: "D:\\work\\app" }]);
  assert.deepEqual(plain(c.folderCrumbs("/home/a")), [{ name: "/", path: "/" }, { name: "home", path: "/home" }, { name: "a", path: "/home/a" }]);
});

test("the same folder, and the home shortened to ~", () => {
  assert.ok(c.sameFolder("D:\\Work\\", "d:/work"));
  assert.ok(!c.sameFolder("/home/A", "/home/a"));
  assert.equal(c.tildeFolder("/Users/a/code/app", "/Users/a"), "~/code/app");
  assert.equal(c.tildeFolder("/Users/a", "/Users/a/"), "~");
  assert.equal(c.tildeFolder("/Users/ab/x", "/Users/a"), "/Users/ab/x");
  assert.equal(c.tildeFolder("C:\\Users\\A\\code", "c:\\users\\a"), "~\\code");
  assert.equal(c.tildeFolder("/tmp", null), "/tmp");
});

test("choices, matching and listings", () => {
  assert.deepEqual(plain(c.folderChoices(["/a/x/", "/b"], ["/a/x", "D:\\W", "d:\\w\\"])), ["/a/x", "/b", "D:\\W"]);
  assert.deepEqual(plain(c.matchFolders(["/code/myapp", "/code/app", "/srv/other"], "app")), ["/code/app", "/code/myapp"]);
  assert.deepEqual(plain(c.matchFolders(["/a", "/b"], " ")), ["/a", "/b"]);
  const entries = [{ name: ".git", kind: "dir" }, { name: "src", kind: "dir" }, { name: "tmp", kind: "link" }, { name: "a.txt", kind: "file" }];
  assert.deepEqual(plain(c.listedFolders(entries)), ["src", "tmp"]);
  assert.deepEqual(plain(c.listedFolders(entries, ".g")), [".git"]);
  assert.deepEqual(plain(c.listedFolders(entries, "SR")), ["src"]);
});

test("the host's home is remembered per host", () => {
  assert.equal(c.knownHome("h"), null);
  c.rememberHome("h", "/Users/a");
  assert.equal(c.knownHome("h"), "/Users/a");
  assert.equal(c.knownHome("other"), null);
});

test("sessions group by folder, latest first", () => {
  const s = (id, cwd, last_activity) => ({ id, cwd, last_activity });
  const g = plain(c.sessionsByFolder([s("a", "/w/x", 1), s("b", "/w/y", 5), s("c", "/w/x/", 9), s("d", "D:\\P", 2), s("e", "d:\\p", 3)]));
  assert.deepEqual(g.map((x) => [x.folder, x.sessions.map((y) => y.id)]), [["/w/x", ["c", "a"]], ["/w/y", ["b"]], ["d:\\p", ["e", "d"]]]);
});

test("how long ago, in one unit", () => {
  const now = 10 * 86400000;
  assert.deepEqual(plain(c.agoParts(now - 30000, now)), { unit: "now", n: 0 });
  assert.deepEqual(plain(c.agoParts(now - 5 * 60000, now)), { unit: "m", n: 5 });
  assert.deepEqual(plain(c.agoParts(now - 3 * 3600000, now)), { unit: "h", n: 3 });
  assert.deepEqual(plain(c.agoParts(now - 2 * 86400000, now)), { unit: "d", n: 2 });
  assert.deepEqual(plain(c.agoParts(now + 5000, now)), { unit: "now", n: 0 });
});

test("what a Windows host's files_list returns (win-dev, 2026-10-22) reads right", () => {
  // `path` values as the host echoed them: ~, a drive root, a folder given with a trailing separator,
  // a forward-slash drive and a UNC share.
  const home = "C:\\Users\\sunjiashuai";
  assert.equal(c.parentFolder(home), "C:\\Users");
  assert.equal(c.tildeFolder(c.childFolder(home, "code"), home), "~\\code");
  assert.equal(c.tildeFolder("C:\\Users\\Administrator\\Desktop", home), "C:\\Users\\Administrator\\Desktop");
  assert.equal(c.childFolder("C:\\Users\\", "lilith"), "C:\\Users\\lilith");
  assert.equal(c.childFolder("D:/", "work"), "D:/work");
  assert.equal(c.folderName("\\\\localhost\\C$"), "C$");
  assert.equal(c.childFolder("\\\\localhost\\C$", "Users"), "\\\\localhost\\C$\\Users");
  assert.equal(c.parentFolder("\\\\localhost\\C$"), null);
  // The host refuses a bare drive; the picker sends it with its root.
  assert.equal(c.trimFolder("C:"), "C:\\");
  // Drive roots list system folders without a hidden flag: they stay out unless asked for.
  const root = ["$GetCurrent", "$RECYCLE.BIN", ".conan", "@editron", "Aomei", "System Volume Information", "Users"].map((name) => ({ name, kind: "dir" }));
  assert.deepEqual(plain(c.listedFolders(root)), ["@editron", "Aomei", "Users"]);
  assert.deepEqual(plain(c.listedFolders(root, "$r")), ["$RECYCLE.BIN"]);
  assert.deepEqual(plain(c.listedFolders(root, "system v")), ["System Volume Information"]);
  assert.deepEqual(plain(c.listedFolders(root, "sy")), []);
});
