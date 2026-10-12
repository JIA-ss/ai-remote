// Decodes the streams `gpu_encode_throughput` wrote (target/stream-bench/*.bin) with WebCodecs in a
// headless Edge or Chrome, the same engine as the desktop app's WebView2, using the codec strings
// the desktop decoder uses. Reports decoded frames and decode throughput per stream.
//
//   node tools/webcodecs-decode-check.mjs [browser.exe]
//
// Only synthetic benchmark pictures are involved. The browser runs with a throwaway profile and is
// stopped at the end.

import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import ts from "../apps/desktop/node_modules/typescript/lib/typescript.js";

const root = new URL("../target/stream-bench/", import.meta.url);
const browser = process.argv[2] ?? "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe";
const files = readdirSync(root).filter((f) => f.endsWith(".bin"));
// The desktop's own codec module, transpiled, so the check uses exactly its codec strings.
const codecModule = ts.transpileModule(readFileSync(new URL("../apps/desktop/src/lib/codec.ts", import.meta.url), "utf8"), {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
}).outputText.replace(/^export /gm, "");
const page = `<!doctype html><script>${codecModule}</script>`;

const server = createServer((req, res) => {
  if (req.url === "/") return res.end(page);
  const name = decodeURIComponent(req.url.slice(1));
  if (!files.includes(name)) return res.writeHead(404).end();
  res.end(readFileSync(new URL(name, root)));
}).listen(0, "127.0.0.1");
await new Promise((r) => server.once("listening", r));
const port = server.address().port;

const profile = mkdtempSync(join(tmpdir(), "ar-webcodecs-"));
const debugPort = 9300 + Math.floor(Math.random() * 600);
const edge = spawn(browser, ["--headless=new", `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`, `http://127.0.0.1:${port}/`], { stdio: "ignore" });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

let target;
for (let i = 0; i < 100 && !target; i++) {
  try {
    target = (await (await fetch(`http://127.0.0.1:${debugPort}/json`)).json()).find((t) => t.type === "page" && t.url.includes(`:${port}`));
  } catch {}
  if (!target) await sleep(200);
}
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let id = 0;
const waiting = new Map();
ws.onmessage = (m) => {
  const d = JSON.parse(m.data);
  waiting.get(d.id)?.(d);
};
const evaluate = (expression) =>
  new Promise((r) => {
    const i = ++id;
    waiting.set(i, r);
    ws.send(JSON.stringify({ id: i, method: "Runtime.evaluate", params: { expression, awaitPromise: true, returnByValue: true } }));
  });

// Wait until the page has defined the codec module.
for (let i = 0; i < 50; i++) {
  const ready = await evaluate("typeof codecString === 'function'");
  if (ready.result?.result?.value) break;
  await sleep(100);
}

const decodeOne = `async (name) => {
  const bytes = new Uint8Array(await (await fetch("/" + name)).arrayBuffer());
  const chunks = [];
  for (let at = 0; at < bytes.length; ) {
    const len = new DataView(bytes.buffer, at + 1, 4).getUint32(0, true);
    chunks.push({ key: bytes[at] === 1, data: bytes.subarray(at + 5, at + 5 + len) });
    at += 5 + len;
  }
  const kind = name.split("-")[0];
  const [codedWidth, codedHeight] = name.replace(".bin", "").split("-")[1].split("x").map(Number);
  const codec = codecString(kind, chunks[0].data);
  let decoded = 0, error = null, width = 0, height = 0, visible = null;
  const done = new Promise((resolve) => {
    const decoder = new VideoDecoder({
      output: (f) => { decoded++; width = f.displayWidth; height = f.displayHeight; visible = f.visibleRect && f.visibleRect.width + "x" + f.visibleRect.height; f.close(); },
      error: (e) => { error = String(e); resolve(); },
    });
    decoder.configure({ codec, codedWidth, codedHeight, optimizeForLatency: true, hardwareAcceleration: "prefer-hardware" });
    const t0 = performance.now();
    chunks.forEach((c, i) => decoder.decode(new EncodedVideoChunk({ type: c.key ? "key" : "delta", timestamp: i * 8333, data: c.data })));
    decoder.flush().then(() => { const ms = performance.now() - t0; resolve(ms); }, (e) => { error = error ?? String(e); resolve(); });
  });
  const ms = await done;
  return { name, codec, chunks: chunks.length, keyframes: chunks.filter((c) => c.key).length, decoded, width, height, visible, error, fps: ms ? Math.round(decoded / (ms / 1000)) : null };
}`;

let failed = false;
for (const name of files) {
  const r = await evaluate(`(${decodeOne})(${JSON.stringify(name)})`);
  const v = r.result?.result?.value;
  if (!v) {
    failed = true;
    console.log(name, JSON.stringify(r.result ?? r).slice(0, 400));
    continue;
  }
  if (v.error || v.decoded !== v.chunks) failed = true;
    const expected = v.name.replace(".bin", "").split("-")[1];
  if (`${v.width}x${v.height}` !== expected) failed = true;
  console.log(`${v.name}: ${v.codec} | ${v.decoded}/${v.chunks} decoded (${v.keyframes} key) display ${v.width}x${v.height} visible ${v.visible} | ~${v.fps} fps decode${v.error ? " | " + v.error : ""}`);
}
ws.close();
edge.kill();
server.close();
await sleep(500);
try { rmSync(profile, { recursive: true, force: true }); } catch {}
process.exit(failed ? 1 : 0);
