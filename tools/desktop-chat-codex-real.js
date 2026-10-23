// The Codex view with a real Codex (tools/desktop-chat-e2e.py --real <name as the panel lists it>, run
// with E2E_SCRIPT=tools/desktop-chat-codex-real.js; E2E_CODEX=<path> offers a Codex binary of your choice
// as "Codex (path)"). Uses that Codex's sign-in and quota. Three turns: a small fix in the work folder
// (reads, an edit, a command, the answer folded behind "Worked for", the changes), a steer while it
// runs commands, and Stop on a long command; then the session is reopened and replays.
(async () => {
  const CFG = __CONFIG__;
  const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);
  const say = (level, message) => invoke("log_from_page", { level, message });
  let failures = 0;
  const T0 = Date.now();
  const check = (name, ok, detail = "") => {
    if (!ok) failures++;
    return say("e2e", `${ok ? "ok  " : "FAIL"}  [${Math.round((Date.now() - T0) / 1000)} s] ${name}${detail ? `  (${String(detail).slice(0, 400)})` : ""}`);
  };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));
  async function until(what, fn, ms = 20000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      try { const v = fn(); if (v) return v; } catch {}
      await sleep(200);
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
  const keyDown = (el, key) => el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
  const text = (el) => (el?.textContent ?? "").replace(/\s+/g, " ").trim();
  const navigationState = () => ({
    route: $(".win")?.dataset.route ?? null,
    chat: $(".nt-chat")?.className ?? null,
    picker: !!$(".nt-chat-start"),
    sessions: $$(".nt-sessions .sess").length,
    menu: !!$(".nt-chat-menu"),
    hidden: document.hidden,
    focus: document.activeElement?.className ?? null,
  });
  // Keep one recovery attempt so replay can still be inspected, but a missed first click is a test
  // failure even if the second works. Preserve its route and DOM state before trying again.
  async function leaveSession() {
    for (let attempt = 1; attempt <= 2; attempt++) {
      const before = navigationState();
      const button = $$(".nt-chat-hd .ico").at(-1);
      if (!button) throw new Error(`session list: menu button missing; ${JSON.stringify({ before })}`);
      button.click();
      const item = await until("session menu", () => $$(".nt-chat-menu [role=menuitem]")[0]);
      if (!item) throw new Error(`session list: menu did not open; ${JSON.stringify({ before, after: navigationState() })}`);
      const connected = item.isConnected;
      item.click();
      const clicked = navigationState();
      const moved = await until("sessions", () => $$(".nt-sessions .sess").length >= 1, 8000);
      await check(`the session list opens on menu click ${attempt}`, !!moved);
      if (moved) return;
      await say("e2e", `     navigation failure: ${JSON.stringify({ attempt, connected, before, clicked, after: navigationState() })}`);
    }
    throw new Error("neither menu click left the conversation");
  }
  const running = () => !!$(".nt-compose .round.stop");
  const turns = () => $$(".cdx-turn");
  const lastTurn = () => turns().at(-1);
  const folderShown = () => $(".nt-chat-start .nt-folder-btn .nt-folder-lbl .fn")?.textContent ?? "";
  async function typeFolder(path) {
    if (!$(".nt-folder-pop")) $(".nt-chat-start .nt-folder-btn").click();
    const q = await until("folder search box", () => $("#ag-folder-q"));
    setValue(q, path);
    await until("open-path row", () => $(".nt-folder-list .row.open"));
    keyDown(q, "Enter");
    const name = path.split(/[\\/]/).filter(Boolean).pop();
    const use = await until("folder listed", () => $(".nt-folder-crumbs .cur")?.textContent === name && $(".nt-folder-ft .btn.primary:not(:disabled)"));
    use?.click();
    await until("picker closed", () => !$(".nt-folder-pop"), 5000);
  }
  async function send(t) {
    const ta = await until("composer", () => $(".nt-compose textarea"));
    setValue(ta, t);
    await sleep(100);
    $(".nt-compose .round.send").click();
  }
  const idle = (ms) => until("turn end", () => !running() && lastTurn() && !lastTurn().classList.contains("running"), ms);
  const openAll = async (turn) => {
    const tg = $(".cdx-worked .x-tg", turn);
    if (tg && tg.getAttribute("aria-expanded") !== "true") tg.click();
    await sleep(300);
    for (const g of $$(".cdx-act.group > .x-hd > .x-ln", turn)) if (g.getAttribute("aria-expanded") !== "true") g.click();
    await sleep(300);
    return $$(".cdx-act > .x-hd > .x-ln", turn).map(text);
  };
  const SHELL = /\/bin\/(ba|z)?sh|\bbash -lc\b|\bzsh -lc\b|powershell(\.exe)? -Command/;

  try {
    await until("start form", () => $(".nt-chat-start"), 30000);
    const pick = $$(".nt-chat-pick .opt").find((b) => b.querySelector(".nm")?.textContent === CFG.agent);
    await check(`${CFG.agent} is offered`, !!pick, $$(".nt-chat-pick .opt").map(text).join(" | "));
    pick.click();
    await typeFolder(CFG.work);
    await check("the work folder is chosen", folderShown() === CFG.work.split(/[\\/]/).pop(), folderShown());
    // E2E_MODEL picks the model (the account's default may not be one this Codex can use).
    if (CFG.model) {
      if (!$("#ag-model")) $(".nt-chat-start .nt-chat-more")?.click();
      const m = await until("model field", () => $("#ag-model"), 5000);
      if (m) setValue(m, CFG.model);
      await check(`the model is set to ${CFG.model}`, m?.value === CFG.model);
    }
    setValue($("#ag-prompt"), "Read export.py. Fix quarter_end so it returns the real last day of the quarter's last month, using calendar.monthrange, and add the imports it needs. "
      + "Then check it by running exactly: python3 -c \"import export; print([str(export.quarter_end(2026, q)) for q in range(1, 5)])\". Reply in two short sentences.");
    await sleep(200);
    $(".nt-chat-start .btn.primary").click();
    await check("the real Codex starts in the Codex view", !!(await until("conversation", () => $(".nt-chat.cdx .nt-chat-hd"), 60000)), $(".nt-chat")?.className);

    // 1. A small fix.
    const live = new Set();
    let folded = false;
    const end = Date.now() + 240000;
    while (Date.now() < end) {
      const t = lastTurn();
      if (t) {
        for (const s of $$(".cdx-act.is-live .cdx-cad > .x-base, .cdx-thinking", t)) live.add(text(s));
        if ($(".cdx-answer.streaming", t) && $(".cdx-worked", t) && !$(".cdx-work", t)) folded = true;
        if (!t.classList.contains("running") && !running()) break;
      }
      await sleep(150);
    }
    const t1 = turns()[0];
    await check("the first turn finishes without failing", !!t1 && !t1.classList.contains("running") && !$(".nt-notice.error", t1), text(t1).slice(-300));
    await check("live lines named what Codex was doing", live.size > 0 && ![...live].some((l) => SHELL.test(l)), [...live].join(" | "));
    await check("the work folded while the answer streamed (or at the end)", folded || (!!$(".cdx-worked", t1) && !$(".cdx-work", t1)));
    await check("'Worked for' and the answer", /Worked for|用时/.test(text($(".cdx-worked", t1))) && text($(".cdx-answer", t1)).length > 10, text($(".cdx-answer", t1)).slice(0, 200));
    const changes = $(".cdx-changes", t1);
    await check("the turn's changes name export.py", !!changes && /export\.py/.test(text(changes)), text(changes));
    if (changes) {
      $(".x-hd .btn", changes).click();
      const d = await until("diff", () => $(".cdx-diff", changes), 3000);
      await check("Review shows its diff, with line numbers and added lines", !!d && $$(".x-dl.x-add", d).length > 0 && $$(".x-no", d).some((n) => /^\d+$/.test(n.textContent)));
    }
    await shot("r01-fix");
    const l1 = await openAll(t1);
    await check("the work has reads or searches as such", l1.some((l) => /^(Read|Searched|Listed|读取了|搜索了|列出了) /.test(l)), l1.join(" | "));
    await check("an edit line for export.py", l1.some((l) => /^(Edited|编辑了) export\.py/.test(l)), l1.join(" | "));
    await check("the check command shows without its shell", l1.some((l) => /^(Ran|运行了) python3 -c/.test(l)) && !l1.some((l) => SHELL.test(l)), l1.join(" | "));
    const ran = $$(".cdx-act", t1).find((a) => /^(Ran|运行了) python3 -c/.test(text($(":scope > .x-hd", a))));
    if (ran) {
      $(":scope > .x-hd > .x-ln", ran).click();
      const sh = await until("shell", () => $(".cdx-shell", ran), 3000);
      await check("its shell shows the output and Success", !!sh && /2026-03-31/.test(text(sh)) && /Success|成功/.test(text($(".x-sh-ft", sh))), text(sh).slice(0, 300));
    }
    await shot("r02-work-open");

    // 2. A steer while it runs commands.
    const n2 = turns().length;
    await send("Run these as separate shell commands, one at a time, and nothing else: sleep 3, then echo one, then sleep 3, then echo two, then sleep 3, then echo three, then sleep 3, then echo four. Then reply with the last word you echoed.");
    await until("turn 2", () => turns().length > n2 && running(), 30000);
    await until("a command runs", () => $$(".cdx-act.is-live", lastTurn()).some((s) => /sleep|echo/.test(text(s))), 60000);
    await send("Change of plan: stop after echo two and reply DONE.");
    const steer = await until("steer bubble", () => $$(".nt-user", lastTurn()).find((u) => u.textContent.includes("Change of plan")), 15000);
    await check("the steer joins the running turn", !!steer && steer.dataset.steer === "1");
    await idle(240000);
    await check("the steered turn ends with an answer", text($(".cdx-answer", lastTurn())).length > 0, text(lastTurn()).slice(-200));
    await check("Codex followed the steer", /DONE/i.test(text($(".cdx-answer", lastTurn()))) && !/four/i.test(text($(".cdx-answer", lastTurn()))), text($(".cdx-answer", lastTurn())));
    await shot("r03-steer");

    // 3. Stop a long command.
    const n3 = turns().length;
    await send("Run exactly this shell command and nothing else: sleep 120");
    await until("turn 3", () => turns().length > n3, 30000);
    const sl = await until("sleep runs", () => $$(".cdx-act.is-live", lastTurn()).some((s) => /sleep 120/.test(text(s))), 90000);
    await check("the long command shows running", !!sl, text(lastTurn()).slice(-200));
    $(".nt-compose .round.stop").click();
    const stopped = await until("stopped", () => $(".cdx-worked.x-stop", lastTurn()), 30000);
    await check("Stop ends the turn and says so", !!stopped, text(lastTurn()).slice(-200));
    await check("it offers to continue", !!$(".nt-stopped .btn", lastTurn()));
    await shot("r04-stopped");

    // 4. Reopen: the conversation replays in the Codex view.
    await leaveSession();
    $$(".nt-sessions .sess").find((s) => s.textContent.includes(CFG.agent)).click();
    const back = await until("replay", () => $(".nt-chat.cdx") && turns().length >= 3 && $(".cdx-worked", turns()[0]), 30000);
    await check("reopening replays it in the Codex view", !!back, `${turns().length} turns`);
    await shot("r05-replay");
  } catch (e) {
    await check("no script errors", false, e && e.stack || e);
  }
  for (const key of ["ai-remote.chat.last", "ai-remote.chat.cwd", "ai-remote.chat.home"]) {
    try {
      const all = JSON.parse(localStorage.getItem(key) ?? "{}");
      for (const h of Object.keys(all)) if (h === CFG.host || /^e2e-/.test(h)) delete all[h];
      localStorage.setItem(key, JSON.stringify(all));
    } catch {}
  }
  await say("e2e", failures ? `${failures} failed` : "all passed");
  await say("done", String(failures));
})();
