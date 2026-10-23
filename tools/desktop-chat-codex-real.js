// The Codex view with a real Codex (tools/desktop-chat-e2e.py --real <name as the panel lists it>, run
// with E2E_SCRIPT=tools/desktop-chat-codex-real.js; E2E_CODEX=<path> offers a Codex binary of your choice
// as "Codex (path)"). Uses that Codex's sign-in and quota. Three turns: a small fix in the work folder
// (reads, an edit, a command, the answer folded behind "Worked for", the changes), a steer while it
// runs commands, and Stop on a long command; then the session is reopened and replays.
(async () => {
  const CFG = __CONFIG__;
  const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);
  const say = (level, message) => invoke("log_from_page", { level, message });
  let failures = 0, checks = 0;
  const T0 = Date.now();
  const check = (name, ok, detail = "") => {
    checks++;
    if (!ok) failures++;
    return say("e2e", `${ok ? "ok  " : "FAIL"}  [${Math.round((Date.now() - T0) / 1000)} s] ${name}${detail ? `  (${String(detail).slice(0, 400)})` : ""}`);
  };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const $ = (sel, root = document) => root?.querySelector(sel) ?? null;
  const $$ = (sel, root = document) => root ? Array.from(root.querySelectorAll(sel)) : [];
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
  const rememberedSession = () => selectedSession();
  const elementLabel = (el) => el ? `${el.tagName ?? "element"}${el.id ? `#${el.id}` : ""}${el.className ? `.${String(el.className).trim().replace(/\s+/g, ".")}` : ""}` : null;
  const elementState = (el) => el ? { element: elementLabel(el), connected: el.isConnected, text: el === document.body ? "" : text(el).slice(0, 160),
    ariaExpanded: el.getAttribute("aria-expanded"), disabled: !!el.disabled, inert: elementLabel(el.closest("[inert]")) } : null;
  const menuButton = () => $(".ws-tb-end .ws-ib[aria-haspopup=menu]");
  const detailedNavigationState = () => ({
    route: $(".win")?.dataset.route ?? null,
    url: window.location.href,
    chat: $(".nt-chat")?.className ?? null,
    chatConnected: $(".nt-chat")?.isConnected ?? false,
    menuButton: elementState(menuButton()),
    menu: !!$(".ws-menu"),
    menuItems: $$(".ws-menu [role=menuitem]").map(elementState),
    startForm: !!$(".ws-new"),
    pickerText: text($(".ws-new")).slice(0, 500),
    sessions: $$(".ws-row").length,
    sessionRows: $$(".ws-row").map(elementState),
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
    const scene = detailedNavigationState();
    const hostSessions = await hostSessionsSnapshot();
    await say("e2e", `     navigation failure: ${JSON.stringify({ reason, ...evidence, scene, hostSessions })}`);
  }
  // Workspace navigation: the real New action and session rows, with no recovery clicks.
  const workspacePanel = () => $(".ws[data-embedded=true]") ?? $(".ws");
  const selectedSession = () => $(".ws-row[data-selected]", workspacePanel())?.dataset.rowKey ?? null;
  const navigationState = () => ({
    route: $(".win")?.dataset.route ?? null,
    chat: $(".nt-chat")?.className ?? null,
    form: !!$(".ws-new", workspacePanel()),
    sessions: $$(".ws-row", workspacePanel()).length,
    selected: selectedSession(),
    sidebar: workspacePanel()?.dataset.side ?? null,
    menu: !!$(".ws-menu"),
    hidden: document.hidden,
    focus: document.activeElement?.className ?? null,
  });
  async function workspaceSidebar() {
    const pane = workspacePanel();
    if (!pane?.isConnected) throw new Error("workspace is missing or disconnected");
    if (!$(".ws-side", pane)) {
      const toggle = $(".ws-corner .ws-ib", pane);
      if (!toggle?.isConnected) throw new Error("workspace sidebar toggle is missing or disconnected");
      toggle.click();
    }
    const sidebar = await until("workspace session sidebar", () => $(".ws-side", workspacePanel()), 5000);
    if (!sidebar) throw new Error("workspace sidebar did not open on its first toggle click");
    return sidebar;
  }
  async function leaveSession(what, ready, options = {}) {
    const state = options.state ?? navigationState;
    const before = state();
    await workspaceSidebar();
    const button = $(".ws-side .ws-nav", workspacePanel());
    if (!button?.isConnected) {
      await options.report?.("navigation-failure", { before, reason: "New action missing or disconnected" });
      throw new Error(`${what}: workspace New action is missing or disconnected`);
    }
    button.click();
    const left = await until("conversation left for workspace New", () => $(".ws-new", workspacePanel()) && !$(".ws-session .nt-chat", workspacePanel()), 8000);
    const evidence = { before, after: state() };
    await check(`${what} leaves the conversation on the first New click`, !!left);
    if (!left) {
      await say("e2e", `     navigation failure: ${JSON.stringify(evidence)}`);
      await options.report?.("navigation-failure", evidence);
      throw new Error(`${what}: the first New click did not leave the conversation`);
    }
    if (!ready()) await say("e2e", `     waiting for ${what} after leaving: ${JSON.stringify(state())}`);
    const loaded = await until(`${what}: workspace content ready`, ready, 30000);
    await check(`${what} content loads after leaving the conversation`, !!loaded);
    if (!loaded) {
      await say("e2e", `     workspace loading failure: ${JSON.stringify(state())}`);
      await options.report?.("workspace-loading-failure", { before, after: state() });
      throw new Error(`${what}: left the conversation but its content did not load within 30 s`);
    }
  }
  async function openWorkspaceSession(key, what = "the session reopens") {
    await workspaceSidebar();
    const row = $$(".ws-row", workspacePanel()).find((candidate) => candidate.dataset.rowKey === key);
    const button = $(".ws-row-main", row);
    if (!row?.isConnected || !button?.isConnected) throw new Error(`${what}: the exact session row is missing or disconnected: ${key}`);
    button.click();
    const opened = await until("the selected workspace conversation", () => selectedSession() === key && $(".ws-session .nt-chat", workspacePanel()), 30000);
    await check(`${what} opens on its first row click`, !!opened);
    if (!opened) throw new Error(`${what}: the first row click did not open ${key}; ${JSON.stringify(navigationState())}`);
    return opened;
  }
  // Workspace startup: reuse its existing CLI chip, project picker, model and first-message composer.
  const folderShown = () => text($(".ws-chip-folder .nt-folder-lbl .fn"));
  async function prepareWorkspace() {
    const pane = await until("complete workspace", workspacePanel, 30000);
    if (!pane) throw new Error("the complete workspace did not appear");
    const divider = $(".nt-live-divider");
    if (divider) {
      // Dock the existing sidebar so its selected row is visible throughout these data-flow checks.
      for (let i = 0; i < 12; i++) { keyDown(divider, "ArrowLeft"); await sleep(20); }
      if (!(await until("workspace sidebar docked", () => workspacePanel()?.dataset.side === "docked", 5000)))
        throw new Error("workspace did not have room for its existing session sidebar");
    }
    if ($(".ws-session .nt-chat", pane)) await leaveSession("the new-session page opens", () => $(".ws-new"));
    if (!(await until("workspace new-session composer", () => $(".ws-new .ws-composer textarea"), 30000)))
      throw new Error("the workspace new-session composer did not appear");
  }
  async function pickAgent(name) {
    const chip = await until("workspace CLI selector", () => $(".ws-env-trail .ws-chip"));
    if (!chip?.isConnected) throw new Error("workspace CLI selector is missing");
    if (text($(".ws-chip-text", chip)) === name) return true;
    chip.click();
    const item = await until(`workspace CLI ${name}`, () => $$(".ws-menu .ws-menu-item").find((choice) => text($(".ws-menu-label", choice)) === name), 5000);
    if (!item?.isConnected) return false;
    item.click();
    return !!(await until("workspace CLI chosen", () => text($(".ws-env-trail .ws-chip .ws-chip-text")) === name, 5000));
  }
  async function typeFolder(path) {
    const name = path.split(/[\\/]/).filter(Boolean).pop();
    if (folderShown() === name) return;
    $(".ws-chip-folder .nt-folder-btn").click();
    const field = await until("folder search box", () => $("#ag-folder-q"));
    if (!field) throw new Error("workspace folder picker did not open");
    setValue(field, path);
    await until("open-path row", () => $(".nt-folder-list .row.open"));
    keyDown(field, "Enter");
    const use = await until("folder listed", () => text($(".nt-folder-crumbs .cur")) === name && $(".nt-folder-ft .btn.primary:not(:disabled)"));
    if (!use) throw new Error(`workspace folder picker did not list ${path}`);
    use.click();
    if (!(await until("folder chosen", () => folderShown() === name && !$(".nt-folder-pop"), 5000)))
      throw new Error(`workspace did not keep the chosen folder ${path}`);
  }
  const running = () => !!$(".nt-compose .round.stop");
  const turns = () => $$(".cdx-turn");
  const lastTurn = () => turns().at(-1);
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
    await prepareWorkspace();
    await check(`${CFG.agent} is offered`, await pickAgent(CFG.agent));
    await typeFolder(CFG.work);
    await check("the work folder is chosen", folderShown() === CFG.work.split(/[\\/]/).pop(), folderShown());
    // E2E_MODEL picks the model (the account's default may not be one this Codex can use).
    if (CFG.model) {
      $(".ws-model > .ws-chip").click();
      const model = await until("workspace model field", () => $(".ws-model-pop input"), 5000);
      if (!model) throw new Error("the existing workspace model field did not open");
      setValue(model, CFG.model);
      await until("model draft rendered", () => model.isConnected && model.value === CFG.model, 5000);
      keyDown(model, "Enter");
      await check(`the model is set to ${CFG.model}`, !!(await until("model chosen", () => text($(".ws-model .ws-chip-text")) === CFG.model, 5000)));
    }
    setValue($(".ws-new .ws-composer textarea"), "Read export.py with the file-reading tool. Use the apply_patch editing tool to fix quarter_end so it returns the real last day of the quarter's last month, using calendar.monthrange, and add the imports it needs. Do not rewrite files with shell or Python commands: this task tests the editing-tool UI. "
      + "Then check it by running exactly: python3 -c \"import export; print([str(export.quarter_end(2026, q)) for q in range(1, 5)])\". Reply in two short sentences.");
    const startButton = await until("first message ready", () => $(".ws-new .ws-send:not(:disabled)"), 5000);
    if (!startButton) throw new Error("the first real Codex message did not become ready");
    startButton.click();
    await check("the real Codex starts in the Codex view", !!(await until("conversation", () => $(".ws-session .nt-chat.cdx"), 60000)), $(".nt-chat")?.className);

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
    await leaveSession("the new-session page opens for replay", () => $$(".ws-row").length >= 1, {
      state: detailedNavigationState, report: navigationFailure,
    });
    const row = $$(".ws-row").find((candidate) => candidate.dataset.rowKey === originalSession);
    const rowBeforeClick = elementState(row);
    await openWorkspaceSession(originalSession, "the original real Codex session");
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
  await say("e2e", `${checks} checks`);
  await say("e2e", failures ? `${failures} failed` : "all passed");
  await say("done", String(failures));
})();
