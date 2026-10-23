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
    // A throttled timer can sleep past the deadline: one last look before giving up.
    try {
      const v = fn();
      if (v) return v;
    } catch {}
    await say("e2e", `     (timed out waiting for ${what})`);
    return null;
  }
  const shotWaits = new Map();
  window.__smokeShot = (name, error) => shotWaits.get(name)?.(error);
  const shot = (name) => new Promise((resolve, reject) => {
    if (shotWaits.has(name)) {
      reject(new Error(`screenshot ${name} is already waiting for its native acknowledgement`));
      return;
    }
    const finish = (error) => {
      if (shotWaits.get(name) !== finish) return;
      clearTimeout(timer);
      shotWaits.delete(name);
      if (error) reject(new Error(`screenshot ${name} failed: ${error}`));
      else resolve();
    };
    const timer = setTimeout(() => finish("native acknowledgement not received within 60000 ms"), 60000);
    shotWaits.set(name, finish);
    // Prepare only the screenshot's finite animation terminal state; live indicators keep running.
    // The page must then stay on this state until the native capture has finished.
    const request = () => {
      if (shotWaits.get(name) === finish) return say("shot", name);
    };
    try {
      Promise.resolve(window.__smokePrepareShot?.(name)).then(request)
        .catch((error) => finish(String(error) || "screenshot preparation or request rejected"));
    } catch (error) {
      finish(String(error) || "screenshot preparation or request rejected");
    }
  });
  function setValue(el, v) {
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, "value").set.call(el, v);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  }
  const keyDown = (el, key) => el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
  const text = (el) => (el?.textContent ?? "").replace(/\s+/g, " ").trim();
  const rememberedSession = () => {
    try { return JSON.parse(localStorage.getItem("ai-remote.chat.last") ?? "{}")[CFG.host] ?? null; }
    catch { return null; }
  };
  const elementLabel = (el) => el ? `${el.tagName ?? "element"}${el.id ? `#${el.id}` : ""}${el.className ? `.${String(el.className).trim().replace(/\s+/g, ".")}` : ""}` : null;
  const elementState = (el) => el ? { element: elementLabel(el), connected: el.isConnected, text: el === document.body ? "" : text(el).slice(0, 160),
    ariaExpanded: el.getAttribute("aria-expanded"), disabled: !!el.disabled, inert: elementLabel(el.closest("[inert]")) } : null;
  const menuButton = () => $(".nt-chat.cdx .nt-chat-hd .nt-chat-menu-btn");
  const navigationState = () => ({
    route: $(".win")?.dataset.route ?? null,
    url: window.location.href,
    chat: $(".nt-chat")?.className ?? null,
    chatConnected: $(".nt-chat")?.isConnected ?? false,
    menuButton: elementState(menuButton()),
    menu: !!$(".nt-chat.cdx .nt-chat-menu"),
    menuItems: $$(".nt-chat.cdx .nt-chat-menu [role=menuitem]").map(elementState),
    picker: !!$(".nt-chat-picker"),
    startForm: !!$(".nt-chat-start"),
    pickerText: text($(".nt-chat-picker, .nt-chat-start")).slice(0, 500),
    sessions: $$(".nt-sessions .sess").length,
    sessionRows: $$(".nt-sessions .sess").map(elementState),
    rememberedSession: rememberedSession(),
    hidden: document.hidden,
    focused: document.hasFocus(),
    activeElement: elementState(document.activeElement),
    dialogs: $$("[role=dialog]").length,
  });
  async function hostSessionsSnapshot() {
    let timer;
    try {
      const reply = await Promise.race([
        invoke("call_tool", { host: CFG.host, tool: "chat_sessions", args: {}, person: true }),
        new Promise((_, reject) => { timer = setTimeout(() => reject(new Error("chat_sessions diagnostic timed out after 10000 ms")), 10000); }),
      ]);
      const body = reply.content?.find((item) => item.type === "text")?.text ?? "";
      if (reply.isError) return { error: body || "chat_sessions refused" };
      return reply.structuredContent ?? JSON.parse(body);
    } catch (error) {
      return { error: String(error) };
    } finally {
      clearTimeout(timer);
    }
  }
  async function navigationFailure(reason, evidence = {}) {
    const scene = navigationState();
    const hostSessions = await hostSessionsSnapshot();
    await say("e2e", `     navigation failure: ${JSON.stringify({ reason, ...evidence, scene, hostSessions })}`);
  }
  // The first failure remains a failure even if one recovery click lets replay checks continue.
  async function leaveSession() {
    for (let attempt = 1; attempt <= 2; attempt++) {
      const before = navigationState();
      const button = menuButton();
      if (!button || !button.isConnected) {
        await check(`the session menu is connected on attempt ${attempt}`, false, JSON.stringify(before));
        await navigationFailure("the Codex session menu button is missing or disconnected", { attempt, before });
        throw new Error("cannot leave the original Codex session: its connected menu button is missing");
      }
      const buttonBeforeClick = elementState(button);
      button.click();
      const items = await until("session menu", () => {
        const choices = $$(".nt-chat.cdx .nt-chat-menu [role=menuitem]");
        return choices.length > 0 && choices;
      });
      const item = items?.find((choice) => /^(Other sessions|其他会话)$/.test(text(choice)));
      if (!item || !item.isConnected) {
        await check(`Other sessions is connected on attempt ${attempt}`, false, items?.map(text).join(" | "));
        await navigationFailure("no connected Other sessions action; no other action was clicked", { attempt, before,
          buttonBeforeClick, buttonAfterClick: elementState(button), choices: items?.map(elementState) ?? [] });
        throw new Error("cannot leave the original Codex session: Other sessions was not available");
      }
      const itemBeforeClick = elementState(item);
      item.click();
      const clicked = navigationState();
      const moved = await until("sessions", () => $$(".nt-sessions .sess").length >= 1, 8000);
      const evidence = { attempt, before, buttonBeforeClick, itemBeforeClick,
        buttonAfterClick: elementState(button), itemAfterClick: elementState(item), clicked, after: navigationState() };
      await say("e2e", `     navigation attempt: ${JSON.stringify(evidence)}`);
      for (const [phase, value] of Object.entries({ before, clicked, after: evidence.after }))
        await say("e2e", `     navigation ${attempt} ${phase}: ${JSON.stringify(value)}`);
      await check(`the session list opens on menu click ${attempt}`, !!moved);
      if (moved) return;
      await navigationFailure("Other sessions did not open a populated session list", evidence);
    }
    throw new Error("neither menu click left the original Codex session for its session list");
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
    setValue($("#ag-prompt"), "Read export.py with the file-reading tool. Use the apply_patch editing tool to fix quarter_end so it returns the real last day of the quarter's last month, using calendar.monthrange, and add the imports it needs. Do not rewrite files with shell or Python commands: this task tests the editing-tool UI. "
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
    const originalSession = rememberedSession();
    if (!originalSession) {
      await navigationFailure("the original session id is missing before leaving");
      throw new Error("cannot verify reopening the original session: no remembered session id");
    }
    await leaveSession();
    const candidates = $$(".nt-sessions .sess").filter((row) => text($(".nm", row)).split(" · ")[0] === CFG.agent);
    if (candidates.length !== 1 || !candidates[0].isConnected) {
      await check("the original agent has one connected session row to reopen", false, `${candidates.length} matches for ${CFG.agent}`);
      await navigationFailure("the original session row is missing, ambiguous or disconnected", { originalSession,
        candidates: candidates.map(elementState) });
      throw new Error("cannot reopen the original session: its session row is missing, ambiguous or disconnected");
    }
    const row = candidates[0], rowBeforeClick = elementState(row);
    row.click();
    const back = await until("replay of the original session", () => rememberedSession() === originalSession
      && $(".nt-chat.cdx") && turns().length >= 3 && $(".cdx-worked", turns()[0]), 30000);
    await check("reopening replays it in the Codex view", !!back,
      `${turns().length} turns; expected session ${originalSession}; actual ${rememberedSession()}`);
    if (!back) {
      await navigationFailure("the original session did not reopen and replay", { originalSession, rowBeforeClick,
        rowAfterClick: elementState(row) });
      throw new Error("the required original session did not reopen in the Codex view");
    }
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
