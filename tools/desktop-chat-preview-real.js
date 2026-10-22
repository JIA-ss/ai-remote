// File previews against a real coding agent on another computer, inside the real app (run by
// tools/desktop-chat-preview-remote.py through the debug build's AI_REMOTE_SMOKE_SCRIPT). The agent
// writes an HTML page, Markdown and an SVG; the harness checks they show in the conversation and times
// fetching them cold (each `preview_file` asks the host for a new ticket and brings the bytes over).
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
    return say("e2e", `${ok ? "ok  " : "FAIL"}  ${name}${detail ? `  (${String(detail).slice(0, 400)})` : ""}`);
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
  const idle = (ms) => until("turn end", () => !running() && !$(".nt-working") && lastTurn()?.querySelector(".nt-foot, .nt-stopped"), ms);
  const ms = (t0) => Math.round(performance.now() - t0);

  try {
    const form = await until("start form", () => $(".nt-chat-start"), 60000);
    await check("the chat panel offers to start an agent", !!form);
    const pick = $$(".nt-chat-pick .opt").find((b) => b.textContent.includes(CFG.agent));
    await check(`${CFG.agent} is installed on the host`, !!pick, $$(".nt-chat-pick .opt").map((b) => b.textContent).join(" | "));
    pick.click();
    const work = CFG.work.replace(/[\\/]+$/, "").split(/[\\/]/).pop();
    const above = CFG.work.replace(/[\\/]+$/, "").slice(0, -work.length);
    $(".nt-chat-start .nt-folder-btn").click();
    const q = await until("folder search", () => $("#ag-folder-q"));
    setValue(q, above);
    await until("open row", () => $(".nt-folder-list .row.open"));
    q.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    (await until("work folder", () => $$(".nt-folder-list .row.child").find((r) => r.querySelector(".fn")?.textContent === work))).click();
    await until("crumbs", () => $(".nt-folder-crumbs .cur")?.textContent === work);
    (await until("use button", () => $(".nt-folder-ft .btn.primary:not(:disabled)"))).click();
    await check("the work folder is chosen", !!(await until("folder chosen", () => $(".nt-chat-start .nt-folder-btn .nt-folder-lbl")?.title)),
      $(".nt-chat-start .nt-folder-btn .nt-folder-lbl")?.title);
    setValue($("#ag-prompt"),
      "Create three files in the folder out/ (create it) with your file-writing tool, not a shell command: " +
      "report.html (a small page: a heading, a table of three rows, and an inline script that writes the current time into the page), " +
      "notes.md (a heading, a short table and a bullet list), and chart.svg (a simple bar chart of three bars). " +
      "Then answer with one line listing the three relative paths, each in backticks.");
    await sleep(200);
    const startBtn = $$(".nt-chat-start .btn.primary").filter((b) => !b.closest(".nt-folder")).pop();
    const t0 = performance.now();
    startBtn.click();
    await check("the turn finishes", !!(await until("conversation", () => $(".nt-chat .nt-chat-hd"), 60000)) && !!(await idle(300000)),
      lastTurn()?.textContent.slice(-300));
    await say("e2e", `     turn took ${ms(t0)} ms`);
    const turn = lastTurn();
    const tEnd = performance.now();
    const cards = await until("inline previews", () => {
      const c = $$(".nt-pvi-f", turn);
      return c.length >= 3 && c.every((x) => x.classList.contains("ready") || x.classList.contains("error")) && c;
    }, 60000);
    await say("e2e", `     inline previews ready ${ms(tEnd)} ms after the turn ended`);
    await check("the three files show inline", !!cards && ["report.html", "notes.md", "chart.svg"].every((n) => cards.some((c) => c.querySelector(".nm")?.textContent === n)),
      $$(".nt-pvi-f", turn).map((x) => `${x.className}:${x.querySelector(".nm")?.textContent}:${x.querySelector(".err")?.textContent ?? ""}`).join(" | "));
    await check("every inline preview loaded", !!cards && cards.every((x) => x.classList.contains("ready")),
      $$(".nt-pvi-f .err", turn).map((e) => e.textContent).join(" | "));
    lastTurn()?.querySelector(".nt-pvi")?.scrollIntoView({ block: "start" });
    await sleep(500);
    await shot("r1-inline");

    // Cold fetches: a new ticket and the bytes each time, nothing cached.
    const session = JSON.parse(localStorage.getItem("ai-remote.chat.last") ?? "{}")[CFG.host];
    const sizes = [];
    for (const f of ["out/report.html", "out/notes.md", "out/chart.svg", ...(CFG.big ? [CFG.big] : [])]) {
      const times = [];
      let bytes = 0;
      for (let i = 0; i < 5; i++) {
        const t = performance.now();
        try {
          const raw = await invoke("preview_file", { host: CFG.host, session, path: f });
          bytes = raw.byteLength ?? raw.length;
          times.push(ms(t));
        } catch (e) {
          times.push(`err ${String(e).slice(0, 120)}`);
        }
      }
      sizes.push(`${f} ${bytes} B: ${times.join(", ")} ms`);
    }
    for (const s of sizes) await say("e2e", `     cold fetch ${s}`);

    // The page with its scripts, in its own window.
    const card = $$(".nt-pvi-f", turn).find((c) => c.querySelector(".nm")?.textContent === "report.html");
    card?.click();
    await until("dialog", () => $(".nt-pv-frame"), 20000);
    await shot("r2-dialog");
    const before = (await invoke("preview_server_requests")).length;
    $(".nt-pv-run")?.click();
    let served = [];
    for (let i = 0; i < 100; i++) {
      served = (await invoke("preview_server_requests")).slice(before);
      if (served.length) break;
      await sleep(100);
    }
    await check("Run scripts serves the page to its own window", served.length > 0 && !$(".nt-pv-note.err"), `${served.join(" ")} ${$(".nt-pv-note.err")?.textContent ?? ""}`);
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
