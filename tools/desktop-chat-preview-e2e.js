// File previews in the desktop app's agent chat panel, end to end inside the real app: the stand-in
// agent's FILES turn writes an HTML page, Markdown and a PNG into the session's folder and a Markdown
// file outside it that it names in a Write (tools/desktop-chat-e2e.py). The bytes come through Tauri →
// controller agent → QUIC → the host's `chat_file` and the files channel.
//
//   E2E_SCRIPT=tools/desktop-chat-preview-e2e.js python3 tools/desktop-chat-e2e.py --out /tmp/chat-preview-e2e
(async () => {
  const CFG = __CONFIG__;
  if (document.visibilityState === "hidden") {
    window.requestAnimationFrame = (cb) => setTimeout(() => cb(performance.now()), 16);
    window.cancelAnimationFrame = (id) => clearTimeout(id);
  }
  const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);
  const say = (level, message) => invoke("log_from_page", { level, message });
  let failures = 0;
  const check = (name, ok, detail = "") => {
    if (!ok) failures++;
    return say("e2e", `${ok ? "ok  " : "FAIL"}  ${name}${detail ? `  (${String(detail).slice(0, 300)})` : ""}`);
  };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));
  async function until(what, fn, ms = 20000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      try {
        const v = fn();
        if (v) return v;
      } catch {}
      await sleep(50);
    }
    await say("e2e", `     (timed out waiting for ${what})`);
    return null;
  }
  const shotWaits = new Map();
  window.__smokeShot = (name) => shotWaits.get(name)?.();
  const shot = (name) => new Promise((r) => { shotWaits.set(name, r); say("shot", name); setTimeout(r, 5000); });
  function setValue(el, v) {
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, "value").set.call(el, v);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  }
  const running = () => !!$(".nt-compose .round.stop");
  const lastTurn = () => $$(".nt-turn").at(-1);
  const idle = (ms = 20000) => until("turn end", () => !running() && !$(".nt-working") && lastTurn()?.querySelector(".nt-foot, .nt-stopped"), ms);
  const dialog = () => $(".nt-pv-dlg");
  const esc = () => window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
  const times = [];
  /** Opens a preview with `click` and waits until its body is ready; returns the dialog. */
  async function open(name, click, ready) {
    const t0 = performance.now();
    click();
    const ok = await until(`${name} preview`, () => dialog() && (ready() || $(".nt-pv-msg.err")), 20000);
    if (ok && !$(".nt-pv-msg.err")) times.push(Math.round(performance.now() - t0));
    return ok;
  }
  // Scripts of a previewed page must not run: one would post this message to the app.
  const posted = [];
  window.addEventListener("message", (e) => posted.push(String(e.data)));

  try {
    const form = await until("start form", () => $(".nt-chat-start"), 30000);
    await check("the chat panel offers to start an agent", !!form);
    $$(".nt-chat-pick .opt").find((b) => b.textContent.includes("Rich fake")).click();
    setValue($("#ag-cwd"), CFG.work);
    setValue($("#ag-prompt"), "FILES make the report");
    await sleep(200);
    $(".nt-chat-start .btn.primary").click();
    await check("the files turn finishes", !!(await until("conversation", () => $(".nt-chat .nt-chat-hd"), 30000)) && !!(await idle(30000)));

    const turn = lastTurn();
    const run = $(".nt-run", turn);
    $(".rh", run).click();
    await sleep(200);
    const eyes = () => $$(".nt-pv-btn", turn).map((b) => b.getAttribute("aria-label"));
    await check("written HTML and Markdown get a preview button, the command does not", eyes().length === 3 &&
      ["report.html", "notes.md", "named.md"].every((n) => eyes().some((l) => l.includes(n))), eyes().join(" | "));
    const named = $$(".md-file", turn).map((e) => e.textContent);
    await check("files named in the reply open previews (a link and inline code)", named.length === 3, named.join(" | "));
    // In the conversation: the turn's files show in place, all fetched from the host.
    const cards = await until("inline previews", () => {
      const c = $$(".nt-pvi-f", turn);
      return c.length === 4 && c.every((x) => x.classList.contains("ready") || x.classList.contains("error")) && c;
    });
    await check("the turn's four files show inline", !!cards, $$(".nt-pvi-f", turn).map((x) => x.className + ":" + x.querySelector(".nm")?.textContent).join(" | "));
    await check("every inline preview loaded", !!cards && cards.every((x) => x.classList.contains("ready")), $$(".nt-pvi-f .err", turn).map((e) => e.textContent).join(" | "));
    const thumb = $(".nt-pvi-f.image img", turn);
    await check("the image shows as a thumbnail", !!(await until("thumbnail", () => thumb?.naturalWidth === 64, 5000)), thumb?.naturalWidth);
    await check("the inline page is sandboxed too", $(".nt-pvi-f.html iframe", turn)?.getAttribute("sandbox") === "");
    await check("the inline Markdown renders its table", !!$(".nt-pvi-f.markdown table", turn));
    await shot("p1-entries");
    $(".nt-pvi-f.html", turn).click();
    await check("clicking an inline preview opens the dialog", !!(await until("dialog", () => $(".nt-pv-frame"), 5000)));
    esc();
    await until("closed", () => !dialog(), 3000);

    // HTML: a sandboxed frame without scripts; nothing from the network.
    const html = await open("HTML", () => $$(".nt-pv-btn", turn).find((b) => b.getAttribute("aria-label").includes("report.html")).click(),
      () => $(".nt-pv-frame"));
    const frame = $(".nt-pv-frame");
    await check("the HTML preview opens in a dialog", !!html && !!frame, $(".nt-pv-msg")?.textContent);
    await check("the frame is sandboxed without scripts", frame?.getAttribute("sandbox") === "", frame?.getAttribute("sandbox"));
    await check("the page is wrapped in a policy that blocks the network", /default-src 'none'/.test(frame?.getAttribute("srcdoc") ?? "") &&
      (frame?.getAttribute("srcdoc") ?? "").indexOf("Content-Security-Policy") < (frame?.getAttribute("srcdoc") ?? "").indexOf("<script>"));
    let opaque = false;
    try {
      opaque = frame.contentDocument === null;
    } catch {
      opaque = true;
    }
    await check("the app cannot reach into the frame (opaque origin) and vice versa", opaque);
    await sleep(1500);
    await check("the page's script did not run", posted.length === 0 && document.title !== "script ran", posted.join(","));
    await shot("p2-html");
    $$(".nt-pv-hd .seg button")[1].click();
    await check("the source view shows the file's text", !!(await until("source", () => $(".nt-pv-src")?.textContent.includes('<h1 id="h">'), 3000)));
    esc();
    await check("Esc closes the preview", !!(await until("closed", () => !dialog(), 3000)));

    // Markdown from inline code in the reply.
    const md = await open("Markdown", () => $$(".md-file", turn).find((e) => e.textContent.includes("notes.md")).click(), () => $(".nt-pv-md table"));
    await check("Markdown renders with its table", !!md && !!$(".nt-pv-md table") && $(".nt-pv-md strong")?.textContent === "bold", $(".nt-pv-msg")?.textContent);
    await shot("p3-markdown");
    esc();
    await until("closed", () => !dialog(), 3000);

    // An image named in the reply (made by a command, not a Write).
    const img = await open("image", () => $$(".md-file", turn).find((e) => e.textContent.includes("chart.png")).click(),
      () => $(".nt-pv-img img")?.naturalWidth > 0);
    const el = $(".nt-pv-img img");
    await check("the PNG decodes at its size", !!img && el?.naturalWidth === 64 && el?.naturalHeight === 32, `${el?.naturalWidth}x${el?.naturalHeight}`);
    await check("the header shows the file's size", /B$/.test($(".nt-pv-hd .sz")?.textContent ?? ""), $(".nt-pv-hd .sz")?.textContent);
    await shot("p4-image");
    $(".nt-pv-dlg button[aria-label]:last-of-type").click();
    await until("closed", () => !dialog(), 3000);

    // Outside the folder: the file the agent named opens, one it did not is refused by the host.
    const out = await open("outside", () => $$(".nt-pv-btn", turn).find((b) => b.getAttribute("aria-label").includes("named.md")).click(),
      () => $(".nt-pv-md h3, .nt-pv-md h1, .nt-pv-md h2"));
    await check("a file outside the folder that the agent wrote can be previewed", !!out && !$(".nt-pv-msg.err"), $(".nt-pv-msg")?.textContent);
    esc();
    await until("closed", () => !dialog(), 3000);
    const session = JSON.parse(localStorage.getItem("ai-remote.chat.last") ?? "{}")[CFG.host];
    const secret = CFG.work.replace(/work$/, "outside/secret.md");
    let refused = "";
    try {
      await invoke("preview_file", { host: CFG.host, session, path: secret });
    } catch (e) {
      refused = String(e);
    }
    await check("a file outside the folder that the agent never named is refused", /outside this session's folder/.test(refused), refused);
    let kind = "";
    try {
      await invoke("preview_file", { host: CFG.host, session, path: CFG.work.replace(/work$/, "host/host/ai-remote.toml") });
    } catch (e) {
      kind = String(e);
    }
    await check("a file of another kind is refused", /cannot be previewed/.test(kind), kind);
    await say("e2e", `     preview open → ready: ${times.join(", ")} ms`);
  } catch (e) {
    await check("no script errors", false, e && e.stack || e);
  }
  for (const key of ["ai-remote.chat.last", "ai-remote.chat.cwd"]) {
    try {
      const all = JSON.parse(localStorage.getItem(key) ?? "{}");
      for (const h of Object.keys(all)) if (h === CFG.host || /^e2e-/.test(h)) delete all[h];
      localStorage.setItem(key, JSON.stringify(all));
    } catch {}
  }
  await say("e2e", failures ? `${failures} failed` : "all passed");
  await say("done", String(failures));
})();
