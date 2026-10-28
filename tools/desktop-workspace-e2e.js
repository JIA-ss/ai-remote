// The agents workspace end to end, inside the real desktop app: run by tools/desktop-chat-e2e.py with
//   E2E_SCRIPT=tools/desktop-workspace-e2e.js E2E_QUERY='view=devices&shotlang=zh&shottheme=dark' \
//     python3 tools/desktop-chat-e2e.py --secs 600 --out /tmp/ws-e2e-zh-dark
// (the same with `shotlang=en&shottheme=light` for the other look). The stand-ins are the harness's:
// "Rich fake" (Claude's stream-json with can_use_tool, titles, patches, LATER / FAIL / EDITAT …), "No-steer
// fake" (the same, messages wait for the turn) and "Codex fake". The chain is the real one: page → Tauri →
// controller agent → QUIC → isolated host → stand-in. Every check is a `[page e2e]` line; screenshots go
// through the smoke hook (`shot(name)`).
//
//   A. The workspace: the computers / agents switch, the new-session page (computer, folder, permission
//      mode, first message), the session's row and its group, the title (first message, then the agent's),
//      the status dots (running, unread, failed + Mark as done, awaiting), pin, rename (F2 and the menu,
//      kept by the host), archive / unarchive, ⌘K by title and by folder, ↑↓ in the list, ⌘B, the edge
//      drag, a Codex session in the Codex view.
//   B. The Claude page in "Ask" mode: approvals (docked over a composer that still types, the first 400 ms
//      ignored, once / for the session / deny), a question in two parts (single, multiple, Other), a plan
//      sent back with a note and one accepted into "Accept edits" (the host's mode event), the mode menu,
//      an edit's diff with its real line numbers, stop and continue, a waiting message edited and taken back.
//   C. First answer wins: a second connection to the same session answers an approval; the desktop's card
//      goes when the host says it was resolved.
//   E. (between A9 and A10) Fork from the row menu: the fork under its parent with the guide line, its
//      agent continuing a copy of the conversation, folding to a summary line, ⌘K finding it by what was
//      said (the hosts' chat_search) and the parent by a title it had before, archiving asking about it.
//   M. Models and effort levels (inside A): the new-session page's model menu and ⌘⇧E, the first message
//      starting with them, the place remembering them, the composer chin's button switching the model in
//      place and the effort level by a restart, and a Codex session's next turn/start carrying both.
//   D. Three idle sessions outside the current conversation: modifier/range selection, the batch
//      menu's keyboard focus, pin/read/archive and Undo; `chat_sessions` independently confirms the
//      host's persisted pin/archive fields, including the archived-only query.
//   E. Groups made on this device with the same sessions: New group named in place, Move to group by
//      digit and by click (with Undo), pointer-event drags into a group (its ring), onto Pinned's drop
//      row and between pinned rows (the insertion line; pins confirmed on the host), out of Pinned, a
//      cancelled drag, group order by drag and ⌥↓, pinned order by ⌥↓, rename and delete (Undo),
//      comfortable rows with the metadata line, and the group's open animation under the motion setting.
(async () => {
  const CFG = __CONFIG__;
  const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);
  const say = (level, message) => invoke("log_from_page", { level, message });
  let failures = 0;
  let checks = 0;
  const check = (name, ok, detail = "") => {
    checks++;
    if (!ok) failures++;
    return say("e2e", `${ok ? "ok  " : "FAIL"}  ${name}${detail ? `  (${String(detail).slice(0, 300)})` : ""}`);
  };
  const note = (text) => say("e2e", `     (note: ${text})`);
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  // A missing root finds nothing (the check that needed it fails, the run goes on).
  const $ = (sel, root = document) => root?.querySelector(sel) ?? null;
  const $$ = (sel, root = document) => (root ? Array.from(root.querySelectorAll(sel)) : []);
  async function until(what, fn, ms = 20000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      try {
        const v = fn();
        if (v) return v;
      } catch {}
      await sleep(100);
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
  const keyDown = (el, key, o = {}) => el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...o }));
  /** A ⌘ shortcut (Ctrl elsewhere), as the window's listeners read it. */
  const mac = /Mac/.test(navigator.platform);
  const chord = (key, o = {}) => keyDown(document.activeElement ?? document.body, key, { code: `Key${key.toUpperCase()}`, ...(mac ? { metaKey: true } : { ctrlKey: true }), ...o });
  const zh = document.documentElement.lang.startsWith("zh");
  const L = (z, e) => (zh ? z : e);
  const text = (el) => (el?.textContent ?? "").trim();

  // The workspace.
  const main = () => $(".ws-main");
  const rowOf = (key) => $(`.ws-row[data-row-key="${CSS.escape(key)}"]`);
  const rowTitle = (key) => text($(".ws-row-title", rowOf(key)));
  const dotOf = (key) => $(".ws-row-dot.ws-dot", rowOf(key))?.dataset.kind ?? null;
  const currentKey = () => $(".ws-row[data-selected]")?.dataset.rowKey ?? null;
  const pageTitle = () => text($(".ws-tb .ws-title"));
  /** A folder group's name (its label is "folder · computer" for another computer). */
  const groupName = (key) => text($(".ws-gh-label", rowOf(key)?.closest(".ws-sec"))).split(" · ")[0];
  const conversation = (key) => currentKey() === key && rowOf(key) && $(".ws-session .nt-chat");
  const readyConversation = (key) => {
    const panel = conversation(key);
    return panel?.isConnected && !$(".nt-chat-status, .cl-loading, .cdx-loading", panel)
      && $(".nt-compose textarea:not(:disabled)", panel) && panel;
  };
  const sceneError = (reason, detail = {}) => new Error(`${reason}: ${JSON.stringify({
    ...detail, current: currentKey(), route: $("[data-route]")?.dataset.route,
    status: text($(".ws-main .nt-chat-status")), outgoing: $$(".ws-main [data-out]").map((el) => ({ state: el.dataset.out, text: text(el) })),
  })}`);
  async function openRow(key) {
    $(".ws-row-main", rowOf(key))?.click();
    const panel = await until(`session ${key} connected and replayed`, () => readyConversation(key));
    if (!panel) throw sceneError("openRow: session never became ready", { key });
    return panel;
  }
  /** Opens a row's menu. A menu closes when the window loses the focus (another app took it): opened again then. */
  async function rowMenu(key) {
    for (let i = 0; i < 4; i++) {
      $(".ws-row-more", rowOf(key))?.click();
      const m = await until("row menu", () => $(".ws-menu .ws-menu-item") && $(".ws-menu"), 1500);
      if (m) {
        if (i) await note(`the row menu opened on try ${i + 1} (window focused: ${document.hasFocus()})`);
        return m;
      }
    }
    return null;
  }
  /** A menu item by its letter (P, U, R, A), as the menus show them. */
  const byLetter = (letter) => $$(".ws-menu .ws-menu-item").find((b) => text($(".ws-menu-key", b)) === letter);
  const closeMenu = () => { const m = $(".ws-menu"); if (m) keyDown(m, "Escape"); };
  async function hostCall(tool, args = {}) {
    const r = await invoke("call_tool", { host: CFG.host, tool, args, person: true });
    const t = r.content?.find((c) => c.type === "text")?.text ?? "";
    if (r.isError) throw new Error(t || `${tool} failed`);
    return r.structuredContent ?? JSON.parse(t);
  }

  // The conversation in the page.
  const composer = () => $(".ws-main .nt-compose textarea");
  const running = () => !!$(".ws-main .nt-compose .round.stop");
  const turns = () => $$(".ws-main .cl-turn");
  const lastTurn = () => turns().at(-1);
  const idle = (ms = 20000) => until("turn end", () => !running() && !$(".ws-main .cl-status") && lastTurn()?.querySelector(".cl-foot, .cl-stopped"), ms);
  async function send(t, { newTurn = false } = {}) {
    const key = currentKey();
    const panel = await until("connected conversation", () => readyConversation(key));
    const ta = $(".nt-compose textarea", panel);
    if (!key || !panel?.isConnected || !ta?.isConnected) throw sceneError("send: no connected composer", { key, message: t });
    const priorTurns = new Set($$(".cl-turn", panel).map((turn) => turn.dataset.turn));
    const priorUsers = new Set($$(".cl-user .txt, .nt-qd .tx", panel));
    setValue(ta, t);
    const button = await until("message ready to send", () => $(".nt-compose .round.send:not(:disabled)", panel));
    if (!button?.isConnected || button.disabled || readyConversation(key) !== panel || composer() !== ta) {
      throw sceneError("send: conversation changed before Send", { key, message: t });
    }
    button.click();
    const accepted = await until(newTurn ? "the submitted message's new turn" : "the submitted message accepted or queued", () => {
      if (conversation(key) !== panel || composer() !== ta || ta.value !== "") return false;
      const candidates = newTurn
        ? $$(".cl-turn", panel).filter((turn) => !priorTurns.has(turn.dataset.turn)).flatMap((turn) => $$(".cl-user .txt", turn))
        : $$(".cl-user .txt, .nt-qd .tx", panel);
      return candidates.find((el) => !priorUsers.has(el) && text(el) === t.trim());
    }, 15000);
    if (!accepted) throw sceneError("send: submitted message was not accepted by the same session", { key, message: t, newTurn });
  }
  const said = (t, ms = 20000) => until(`reply "${t}"`, () => lastTurn()?.textContent.includes(t), ms);

  // The new-session page.
  const workName = CFG.work.split(/[\\/]/).pop();
  const sep = CFG.work.includes("\\") ? "\\" : "/";
  const above = CFG.work.slice(0, CFG.work.length - workName.length - 1);
  async function pickAgent(name) {
    const chip = await until("agent chip", () => $(".ws-env-trail .ws-chip"));
    if (text($(".ws-chip-text", chip)) === name) return true;
    chip.click();
    const item = await until(`agent ${name}`, () => $$(".ws-menu .ws-menu-item").find((b) => text($(".ws-menu-label", b)) === name), 5000);
    item?.click();
    return until("agent chosen", () => text($(".ws-env-trail .ws-chip .ws-chip-text")) === name, 5000);
  }
  const folderShown = () => text($(".ws-chip-folder .nt-folder-lbl .fn"));
  /** Browses to `path` in the folder picker (a typed path, then a subfolder `child` if given, then "Use"). */
  async function browseTo(path, name, child = null) {
    $(".ws-chip-folder .nt-folder-btn").click();
    const q = await until("folder field", () => $("#ag-folder-q"));
    setValue(q, path);
    await until("open row", () => $(".nt-folder-list .row.open"));
    keyDown(q, "Enter");
    if (child) {
      const row = await until("subfolder", () => $$(".nt-folder-list .row.child").find((r) => text($(".fn", r)) === child));
      row?.click();
    }
    await until("crumbs", () => text($(".nt-folder-crumbs .cur")) === name);
    const use = await until("use button", () => $(".nt-folder-ft .btn.primary:not(:disabled)"));
    use?.click();
    return until("folder chosen", () => folderShown() === name, 5000);
  }
  async function pickRecent(name) {
    if (folderShown() === name) return true;
    $(".ws-chip-folder .nt-folder-btn").click();
    const row = await until("recent folder", () => $$(".nt-folder-list .row.recent").find((r) => text($(".fn", r)) === name));
    row?.click();
    return until("folder chosen", () => folderShown() === name, 5000);
  }
  async function pickMode(index) {
    $$(".ws-env-lead > button.ws-chip")[1].click();
    const items = await until("mode menu", () => $$(".ws-menu [role=menuitemradio]").length === 4 && $$(".ws-menu [role=menuitemradio]"), 5000);
    items?.[index].click();
    await sleep(100);
    return text($(".ws-chip-text", $$(".ws-env-lead > button.ws-chip")[1]));
  }
  async function newSession() {
    $(".ws-side .ws-nav").click();
    return until("new-session page", () => $(".ws-new .ws-composer textarea"));
  }
  /** Sends the first message from the new-session page; resolves to the new session's row key. */
  async function start(prompt) {
    const ta = $(".ws-new .ws-composer textarea");
    setValue(ta, prompt);
    await sleep(150);
    $(".ws-new .ws-send").click();
    const page = await until("session page", () => $(".ws-session .nt-chat") && currentKey(), 30000);
    return page ? currentKey() : null;
  }

  // What this run changes in the webview's storage (shared with an installed app): put back at the end.
  const KEEP = /^ai-remote\.(mode$|ws\.|live\.split$)/;
  // Leftovers of earlier runs (hosts named e2e-<pid>, gone now) are dropped from it first.
  const leftover = (x) => typeof x === "string" && /^e2e-\d+(\/|$)/.test(x);
  for (const k of Object.keys(localStorage).filter((k) => KEEP.test(k))) {
    let v;
    try {
      v = JSON.parse(localStorage.getItem(k));
    } catch {
      continue;
    }
    let changed = false;
    if (Array.isArray(v)) {
      const kept = v.filter((x) => !leftover(x) && !(x && typeof x === "object" && leftover(x.host)));
      changed = kept.length !== v.length;
      v = kept;
    } else if (v && typeof v === "object") {
      for (const key of Object.keys(v)) if (leftover(key)) { delete v[key]; changed = true; }
    }
    if (changed) localStorage.setItem(k, JSON.stringify(v));
  }
  const saved = Object.fromEntries(Object.keys(localStorage).filter((k) => KEEP.test(k)).map((k) => [k, localStorage.getItem(k)]));
  // Diagnostics: say when the window leaves the workspace without the script asking for it.
  let leaving = false;
  let inWs = false;
  setInterval(() => {
    const now = !!$(".ws");
    if (inWs && !now && !leaving) note(`the workspace was replaced by another page (${text($("h1, .hd, .ttl")).slice(0, 60)})`);
    inWs = now;
  }, 50);
  try {
    // A fresh layout: the sidebar at its default width, nothing folded, no filter.
    localStorage.setItem("ai-remote.ws.side", JSON.stringify({ w: 288, hidden: false }));
    localStorage.setItem("ai-remote.ws.collapsed", "[]");
    localStorage.removeItem("ai-remote.ws.list");
    localStorage.removeItem("ai-remote.ws.dismissed");

    // ===== A. The workspace =====
    const sw = await until("the computers page's switch", () => $(".ws-modes [data-mode=agents]"), 30000);
    await check("the computers page has the computers / agents switch", !!sw && !$(".ws"));
    sw.click();
    const ws = await until("workspace", () => $(".nt-live-workspace .ws .ws-new"), 10000);
    await check("the switch opens the agents workspace on a new session", !!ws && localStorage.getItem("ai-remote.mode") === "agents", localStorage.getItem("ai-remote.mode"));
    await check("Agents opens the complete workspace beside one live screen", $$(".nt-live-workspace .nt-stage canvas.rd-holder").length === 1
      && $$(".nt-live-workspace .ws[data-embedded=true]").length === 1);
    // Give the existing sidebar enough space for the data-flow checks below. A narrow pane's overlay
    // intentionally unmounts its rows on selection; container folding is checked at the end.
    const split = await until("live workspace divider", () => $(".nt-live-divider"));
    for (let i = 0; i < 12; i++) { keyDown(split, "ArrowLeft"); await sleep(20); }
    await until("workspace sidebar docked", () => $(".ws")?.dataset.side === "docked", 5000);
    await check("the sidebar is docked at 288 px", $(".ws")?.dataset.side === "docked" && Math.round($(".ws-side").getBoundingClientRect().width) === 288,
      `${$(".ws")?.dataset.side} ${$(".ws-side")?.getBoundingClientRect().width}`);

    // A1. The new-session page: computer, agent, folder (browsed), permission mode, first message.
    await until("computers", () => text($(".ws-env-lead .ws-chip .ws-chip-text")) === CFG.host, 15000);
    $(".ws-env-lead > button.ws-chip").click();
    const hostItem = await until("environment menu", () => $$(".ws-menu [role=menuitemradio]").find((b) => text($(".ws-menu-label", b)) === CFG.host), 5000);
    await check("the environment menu lists the computer, chosen", !!hostItem && hostItem.getAttribute("aria-checked") === "true", $(".ws-menu")?.textContent);
    hostItem?.click();
    await check("the agent menu offers the stand-ins", await pickAgent("Rich fake"));
    await check("with no folder yet the page asks for one", !!$(".ws-chip-folder .nt-folder-lbl.none") || /选择文件夹|Choose a folder/.test(text($(".ws-new .ws-cards"))), text($(".ws-chip-folder")));
    await check("the folder picker browses the computer and takes the work folder", !!(await browseTo(above, workName, workName)), folderShown());
    const modeText = await pickMode(0);
    await check("the permission mode can be set to Ask", /询问|Ask/.test(modeText), modeText);
    // M1. NS-32 / NS-33: the model chip opens a menu (Claude Code's aliases before the agent has run on
    // this computer) and ⌘⇧E opens it at the effort levels; both go with the first message.
    const modelChip = () => $(".ws-env-trail .ws-model-chip");
    const chipWords = (chip) => `${text($(".ws-chip-text", chip))}|${text($(".ws-chip-sub", chip))}`;
    modelChip()?.click();
    const haiku = await until("model menu", () => $$(".ws-menu [role=menuitemradio]").find((b) => /^Haiku/.test(text($(".ws-menu-label", b)))), 5000);
    await check("M1 the model chip opens a menu of models with a custom ID field", !!haiku && !!$(".ws-menu .ws-menu-field input"),
      text($(".ws-menu")).slice(0, 200));
    haiku?.click();
    await until("model menu closed", () => !$(".ws-menu"), 3000);
    $(".ws-new .ws-composer textarea")?.focus();
    chord("e", { shiftKey: true });
    const high = await until("effort levels", () => document.activeElement?.closest(".ws-menu")
      && $$(".ws-menu [role=menuitemradio]").find((b) => text($(".ws-menu-label", b)) === L("高", "High")), 5000);
    await check("M1 ⌘⇧E opens the menu at the effort levels", !!high && /默认|Default/.test(text(document.activeElement)), text(document.activeElement));
    high?.click();
    await check("M1 the chip shows the model and the effort word", !!(await until("model chip", () => chipWords(modelChip()) === `Haiku|${L("高", "High")}`, 3000)),
      chipWords(modelChip()));
    await shot("a01-new-session");
    const prompt1 = "DELAYTITLE LATER6 整理季度导出脚本";
    const k1 = await start(prompt1);
    await check("sending the first message opens the session", !!k1, k1);
    await check("the first message shows as the person's", !!(await until("user row", () => $$(".ws-main .nt-user").some((u) => u.textContent.includes(prompt1)))));
    const group1 = await until("project group", () => rowOf(k1)?.closest(".ws-sec"));
    await check("the sidebar groups the session under its folder", !!group1 && groupName(k1) === workName, text($(".ws-gh-label", group1)));
    await check("the title starts as the first message", rowTitle(k1).includes("整理季度导出脚本") && pageTitle().includes("整理季度导出脚本"), `${rowTitle(k1)} | ${pageTitle()}`);
    const blink = await until("running dot", () => dotOf(k1) === "running" && $(".ws-row-dot", rowOf(k1)));
    const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;
    const dotAnimation = blink && getComputedStyle(blink).animationName;
    await check("a running session's dot respects the system's motion setting", !!blink && (reducedMotion ? dotAnimation === "none" : dotAnimation !== "none"),
      `reduced motion: ${reducedMotion}; animation: ${dotAnimation}`);
    await shot("a02-running");
    // The stand-in answers the title 4 s after it is asked; the row and the page learn it from the next poll.
    const sentAt = Date.now();
    const titled = await until("agent title", () => rowTitle(k1).startsWith("Fake:") && pageTitle().startsWith("Fake:"), 30000);
    if (titled) await note(`the agent's title showed ${((Date.now() - sentAt) / 1000).toFixed(1)} s after the first message`);
    await check("the agent's title replaces it", !!titled, `${rowTitle(k1)} | ${pageTitle()}`);
    await check("the turn ends by itself", !!(await said("later: DELAYTITLE", 20000)));
    const m1 = (await hostCall("chat_sessions")).sessions.find((x) => k1?.endsWith(`/${x.id}`));
    await check("M1 the session started with the chosen model and effort level", m1?.model === "haiku" && m1?.effort === "high",
      JSON.stringify({ model: m1?.model, effort: m1?.effort }));
    // An occluded/locked desktop polls at the store's idle 20 s cadence, instead of 4 s. Observe
    // the natural poll; do not force a refresh or fail before that configured interval can elapse.
    const settledAt = Date.now();
    await check("the ended session has no dot (it is open)", !!(await until("no dot", () => dotOf(k1) === null, 35000)),
      `${dotOf(k1)}; ${(Date.now() - settledAt) / 1000} s; visibility: ${document.visibilityState}`);

    // M2. CP-53: the composer chin's right side: the model (the agent's own list now) and the effort word,
    // then the context ring. A model switches in the running process; another level restarts it.
    const chin = () => $(".ws-main .cl-model-chip");
    await check("M2 the chin shows the model's name and its effort word", !!(await until("chin model", () => chipWords(chin()) === `Haiku 5.5|${L("高", "High")}`, 15000)),
      chipWords(chin()));
    const chinBox = chin()?.getBoundingClientRect();
    const modeBox = $(".ws-main .nt-compose-ft .cl-mode")?.getBoundingClientRect();
    await check("M2 the model sits right of the permission mode, inside the composer's width", !!chinBox && !!modeBox && chinBox.left > modeBox.right
      && chinBox.right <= ($(".ws-main .nt-compose")?.getBoundingClientRect().right ?? 0) + 1, `${chinBox?.left} ${modeBox?.right}`);
    await send("WHICH 1");
    await check("M2 the agent runs with the chosen model and level", !!(await said("model=haiku effort=high")));
    chin()?.click();
    const opus = await until("session model menu", () => $$(".ws-menu [role=menuitemradio]").find((b) => text($(".ws-menu-label", b)) === "Opus 5.5"), 5000);
    await check("M2 its menu is titled for this session and lists the agent's models", !!opus && /仅本会话|this session/.test(text($(".ws-menu .ws-menu-h"))),
      text($(".ws-menu")).slice(0, 160));
    const pop = $(".ws-menu")?.getBoundingClientRect();
    await check("M2 the menu stays inside the window", !!pop && pop.top >= 0 && pop.left >= 0 && pop.bottom <= innerHeight && pop.right <= innerWidth,
      pop && `${pop.left},${pop.top},${pop.right},${pop.bottom} / ${innerWidth}x${innerHeight}`);
    await shot("a01b-model-menu");
    opus?.click();
    await check("M2 a model chosen mid-session shows once the host says it holds", !!(await until("chin opus", () => text($(".ws-chip-text", chin())) === "Opus 5.5", 10000)),
      chipWords(chin()));
    await send("WHICH 2");
    await check("M2 the running agent switched in place", !!(await said("model=opus effort=high")));
    composer()?.focus();
    chord("e", { shiftKey: true });
    const low = await until("effort item", () => document.activeElement?.closest(".ws-menu") && $$(".ws-menu [role=menuitemradio]").find((b) => text($(".ws-menu-label", b)) === L("低", "Low")), 5000);
    await check("M2 ⌘⇧E opens it at the effort levels, the current one focused", !!low && text(document.activeElement) === L("高", "High"), text(document.activeElement));
    low?.click();
    await check("M2 another effort level shows", !!(await until("chin low", () => text($(".ws-chip-sub", chin())) === L("低", "Low"), 10000)), chipWords(chin()));
    await send("WHICH 3");
    await check("M2 the agent restarted with it and the conversation goes on", !!(await said("model=opus effort=low", 30000)));
    await idle();

    // A2. Unread: a session that moves on while another is open gets a blue dot; opening it clears it.
    await newSession();
    await check("New keeps the last place", folderShown() === workName, folderShown());
    await check("M3 the place remembers the model and level it started with (NS-29)", !!(await until("remembered model", () =>
      /^Haiku/.test(text($(".ws-chip-text", modelChip()))) && text($(".ws-chip-sub", modelChip())) === L("高", "High"), 5000)), chipWords(modelChip()));
    const k2 = await start("LATER25 第二个会话");
    await check("a second session opens", !!k2 && k2 !== k1, k2);
    await openRow(k1);
    await check("its dot runs while the other is open", dotOf(k2) === "running", dotOf(k2));
    const unread = await until("unread dot", () => dotOf(k2) === "unread", 45000);
    await check("it ends while away: blue unread dot", !!unread, dotOf(k2));
    await shot("a03-unread");
    await openRow(k2);
    await check("opening it clears the dot", !!(await until("read", () => dotOf(k2) === null, 10000)), dotOf(k2));

    // A3. A failed turn: red dot; Mark as done takes it away.
    const failedAt = Date.now();
    const failedSnapshot = async (label) => {
      const report = await hostCall("chat_sessions");
      const m = report.sessions.find((x) => x.id === k2.slice(k2.indexOf("/") + 1));
      await note(`failed-turn ${label} ${JSON.stringify({
        elapsedMs: Date.now() - failedAt, visibility: document.visibilityState, focused: document.hasFocus(), dot: dotOf(k2),
        connectionStatus: text($(".ws-main .nt-chat-status")),
        outgoing: $$(".ws-main [data-out]").map((x) => ({ state: x.dataset.out, error: $(".st.bad", x)?.title ?? null })),
        host: m ? { state: m.state, turns: m.turns, last_status: m.last_status, seq: m.seq, last_seq: m.last_seq, last_activity: m.last_activity, pending: m.pending } : null
      })}`);
    };
    await failedSnapshot("before-send");
    await send("FAIL 这一轮会失败", { newTurn: true });
    await failedSnapshot("sent");
    await check("a failed turn shows a red dot", !!(await until("failed dot", () => dotOf(k2) === "failed", 20000)), dotOf(k2));
    await failedSnapshot("after-dot-wait");
    await shot("a04-failed");
    await rowMenu(k2);
    const done = byLetter("U");
    const offersDone = !!done?.isConnected && /标记为完成|Mark as done/.test(text(done));
    await check("its menu offers Mark as done", offersDone, text(done));
    if (!offersDone) throw sceneError("Mark as done: expected action is unavailable", { key: k2 });
    const donePanel = conversation(k2);
    const priorToasts = new Set($$(".ws-toast"));
    let leftDonePage = !donePanel;
    const sameDonePage = () => conversation(k2) === donePanel && !!donePanel?.isConnected;
    const donePageWatch = new MutationObserver(() => { if (!sameDonePage()) leftDonePage = true; });
    donePageWatch.observe(document.body, { childList: true, subtree: true });
    if (!done.isConnected || !sameDonePage() || !/标记为完成|Mark as done/.test(text(done))) {
      donePageWatch.disconnect();
      throw sceneError("Mark as done: menu or conversation changed before the click", { key: k2 });
    }
    done.click();
    const cleared = await until("done on the same session page", () => !leftDonePage && sameDonePage() && rowOf(k2) && dotOf(k2) === null, 5000);
    await check("Mark as done clears the red dot", !!cleared && !leftDonePage, dotOf(k2));
    const doneToast = await until("Mark as done toast on the same session page", () => !leftDonePage && sameDonePage()
      && $$(".ws-toast").find((el) => !priorToasts.has(el) && /已把.*标记为完成|marked as done/.test(text(el)) && $(".ws-toast-act", el)), 3000);
    donePageWatch.disconnect();
    await check("and says so in a toast", !!doneToast && !leftDonePage);

    // A4. Pin and unpin.
    await rowMenu(k1);
    byLetter("P")?.click();
    const pinnedSec = await until("pinned section", () => rowOf(k1)?.closest('.ws-sec[data-group="pinned"]'), 10000);
    await check("pinning moves the row into Pinned", !!pinnedSec, $$(".ws-sec").map((s) => s.dataset.group).join(","));
    await shot("a05-pinned");
    await rowMenu(k1);
    const unpin = byLetter("P");
    await check("the pinned row's menu offers Unpin", /取消置顶|Unpin/.test(text(unpin)), text(unpin));
    unpin?.click();
    await check("unpinning removes the Pinned section", !!(await until("no pinned", () => !$('.ws-sec[data-group="pinned"]'), 10000)));

    // A5. Rename: F2 on a row, then the menu; the host keeps it.
    $(".ws-row-main", rowOf(k2))?.focus();
    keyDown(document.activeElement, "F2");
    const field = await until("rename field", () => $(".ws-rename", rowOf(k2)), 5000);
    await check("F2 on a row opens its name for editing, all selected", !!field && field.selectionStart === 0 && field.selectionEnd === field.value.length, field?.value);
    const name2 = "季度导出重命名";
    setValue(field, name2);
    keyDown(field, "Enter");
    await check("Enter keeps the new name", !!(await until("renamed", () => rowTitle(k2) === name2 && pageTitle() === name2, 10000)), `${rowTitle(k2)} | ${pageTitle()}`);
    await sleep(5000); // a poll
    await check("renaming does not bring back the red dot put aside with Mark as done", !!doneToast && !leftDonePage && !!rowOf(k2)
      && conversation(k2) === donePanel && dotOf(k2) !== "failed", dotOf(k2));
    const listed = await hostCall("chat_sessions").catch((e) => ({ error: String(e) }));
    await check("the host has the new name (chat_update)", JSON.stringify(listed).includes(name2), JSON.stringify(listed).slice(0, 200));
    await rowMenu(k1);
    byLetter("R")?.click();
    const field1 = await until("rename from the menu", () => $(".ws-rename", rowOf(k1)), 5000);
    const name1 = "季度导出：第一个";
    if (field1) {
      setValue(field1, name1);
      keyDown(field1, "Enter");
    }
    await check("the menu's Rename works the same way", !!(await until("renamed 1", () => rowTitle(k1) === name1, 10000)), rowTitle(k1));
    // Out of the workspace and back: the names come from the host again.
    leaving = true;
    $(".ws-modes [data-mode=computers]").click();
    await until("computers page", () => !$(".ws") && $(".ws-modes [data-mode=agents]"), 10000);
    $(".ws-modes [data-mode=agents]").click();
    await until("workspace again", () => rowOf(k2), 10000);
    leaving = false;
    await check("the names are still there after leaving and coming back", rowTitle(k2) === name2 && rowTitle(k1) === name1, `${rowTitle(k1)} | ${rowTitle(k2)}`);

    // A6. Archive and unarchive (from the list's "archived" view).
    await openRow(k1);
    await rowMenu(k2);
    byLetter("A")?.click();
    await check("archiving takes the row out of the list", !!(await until("archived", () => !rowOf(k2), 10000)));
    // The row disappears optimistically; the Undo toast follows the host's completed update.
    await check("with a toast that can undo it", !!(await until("archive Undo toast", () => $$(".ws-toast").find((x) => $(".ws-toast-act", x)), 10000)));
    const filter = () => $$(".ws-menu [role=menuitemradio]");
    // The "Show" choices by their labels (the menu also lists groupings, order and row density).
    const show = (which) => filter().find((b) => ({ active: /^(未归档的|Not archived)$/, archived: /^(已归档的|Archived)$/, all: /^(全部会话|All sessions)$/ })[which]
      .test(text($(".ws-menu-label", b))));
    $(".ws-list-filter").click();
    (await until("list menu", () => show("archived"), 5000))?.click();
    const back = await until("archived row", () => rowOf(k2)?.hasAttribute("data-archived") && rowOf(k2), 10000);
    await check("the archived view lists it, marked", !!back);
    await sleep(5000); // a poll with the archived list
    await check("the open session's page stays while the list shows archived sessions", !!$(".ws-session .nt-chat") && !$(".ws-session .ws-state"),
      text($(".ws-session .ws-state")) || "");
    await shot("a06-archived");
    show("all")?.click();
    await sleep(5000);
    await check("the All view lists archived and other sessions together", !!rowOf(k2) && !!rowOf(k1), `${!!rowOf(k1)} ${!!rowOf(k2)}`);
    show("archived")?.click();
    await until("archived row", () => rowOf(k2), 10000);
    closeMenu();
    await until("menu closed", () => !$(".ws-menu"), 3000);
    await rowMenu(k2);
    const unarch = await until("Unarchive item", () => byLetter("A"), 3000);
    await check("an archived row's menu offers Unarchive", /取消归档|Unarchive/.test(text(unarch)), $(".ws-menu")?.textContent);
    unarch?.click();
    await check("Unarchive takes it out of the archived view, with a toast", !!(await until("unarchived", () => !rowOf(k2) && $$(".ws-toast").some((x) => /取消归档|Unarchived/.test(x.textContent)), 10000)));
    $(".ws-list-filter").click();
    (await until("list menu", () => show("active"), 5000))?.click();
    closeMenu();
    await check("the active view has it again", !!(await until("active again", () => rowOf(k2) && !rowOf(k2).hasAttribute("data-archived") && !$(".ws-list-filter.on"), 10000)));
    await openRow(k1);

    // A7. A Codex session (in a subfolder): the Codex view.
    await newSession();
    await check("the agent menu switches to Codex", await pickAgent("Codex fake"));
    await check("the picker takes a typed subfolder", !!(await browseTo(`${CFG.work}${sep}src`, "src")), folderShown());
    await check("Codex has no permission-mode chip", $$(".ws-env-lead > button.ws-chip").length === 1);
    const k3 = await start("hello codex");
    await check("a Codex session gets the Codex view", !!(await until("codex view", () => $(".ws-main .nt-chat.cdx"), 20000)), $(".ws-main .nt-chat")?.className);
    await check("in its own folder's group", groupName(k3) === "src", text($(".ws-gh-label", rowOf(k3)?.closest(".ws-sec"))));
    await until("codex turn", () => $(".ws-main .cdx-turn"), 15000);
    const codexComposer = $(".ws-main .nt-chat.cdx .nt-compose")?.getBoundingClientRect();
    await check("the workspace Codex composer keeps its whole lower edge above the window bottom",
      !!codexComposer && codexComposer.bottom <= innerHeight - 12, codexComposer && `${codexComposer.bottom} / ${innerHeight}`);
    await shot("a07-codex");

    // M4. Codex: the host lists the models model/list gave, and a model and level chosen go with the next
    // turn/start (Codex keeps them; back to the default sends the thread's own). The Codex view's own
    // composer belongs to another piece of work, so the command goes over a second connection here.
    const cx = { events: [] };
    try {
      const att = await hostCall("chat_attach", { session: k3.split("/").pop() });
      const cb = window.__TAURI_INTERNALS__.transformCallback((raw) => { if (raw && "message" in raw) cx.events.push(raw.message); });
      const channel = { toJSON: () => `__CHANNEL__:${cb}`, __TAURI_TO_IPC_KEY__: () => `__CHANNEL__:${cb}` };
      const [id, hello] = await invoke("open_chat", { host: CFG.host, request: { ticket: att.ticket, since: 0 }, onEvent: channel });
      Object.assign(cx, { id, hello });
    } catch (e) {
      cx.error = String(e && e.message || e);
    }
    await check("M4 a Codex session's hello lists its models (hidden ones left out)", (cx.hello?.models ?? []).map((m) => m.id).join() === "gpt-fake,gpt-fake-mini",
      cx.error ?? JSON.stringify(cx.hello?.models ?? null).slice(0, 200));
    if (cx.id !== undefined) {
      const turnText = async (what, ms = 20000) => until(what, () => cx.events.find((e) => e?.t === "turn_end" && (e.final_text ?? "").includes(what)), ms);
      await invoke("chat_send", { id: cx.id, command: { model: { model: "gpt-fake-mini", effort: "high" } } });
      await check("M4 the host takes the model and level for the next turn", !!(await until("codex model event", () =>
        cx.events.find((e) => e?.t === "model" && e.model === "gpt-fake-mini" && e.effort === "high"), 10000)));
      await invoke("chat_send", { id: cx.id, command: { input: { id: `w${Date.now()}`, text: "WHICH mini" } } });
      await check("M4 turn/start carries the chosen model and effort level", !!(await turnText("model=gpt-fake-mini effort=high")));
      await invoke("chat_send", { id: cx.id, command: { model: { model: null, effort: null } } });
      await invoke("chat_send", { id: cx.id, command: { input: { id: `w${Date.now()}`, text: "WHICH default" } } });
      await check("M4 back to the default sends the thread's own model and level", !!(await turnText("model=gpt-fake effort=medium")));
      await invoke("close_chat", { id: cx.id }).catch(() => {});
    }
    // The new-session page now offers Codex's own models and levels (asked again after a start).
    await newSession();
    await pickAgent("Codex fake");
    modelChip()?.click();
    const mini = await until("codex models", () => $$(".ws-menu [role=menuitemradio]").find((b) => text($(".ws-menu-label", b)) === "GPT Fake Mini"), 10000);
    await check("M4 the new-session page lists Codex's models after it ran here", !!mini, text($(".ws-menu")).slice(0, 200));
    mini?.click();
    await until("menu closed", () => !$(".ws-menu"), 3000);
    modelChip()?.click();
    const levels = await until("codex levels", () => $(".ws-menu") && $$(".ws-menu [role=menuitemradio]").map((b) => text($(".ws-menu-label", b))), 5000);
    await check("M4 only the levels that model takes are offered, its own default named", !!levels && levels.includes(L("高", "High"))
      && !levels.includes(L("中", "Medium")) && levels.some((l) => /低|Low/.test(l) && /默认|Default/.test(l)), (levels ?? []).join(" | "));
    await shot("a07b-codex-model-menu");
    closeMenu();
    await until("menu closed", () => !$(".ws-menu"), 3000);

    // A8. ⌘K: by title, by folder; Enter opens.
    chord("k");
    const pq = await until("palette", () => $(".ws-palette input"), 5000);
    await check("⌘K opens the search", !!pq);
    setValue(pq, "重命名");
    const mainRows = () => $$(".ws-palette-row:not([data-content-hit])");
    const hit = await until("title hit", () => mainRows().length === 1 && text($(".ws-palette-title", mainRows()[0])) === name2, 5000);
    await check("it finds a session by its title", !!hit, $$(".ws-palette-row").map(text).join(" | "));
    await shot("a08-search");
    keyDown(pq, "Enter");
    await check("Enter opens it", !!(await until("opened", () => !$(".ws-palette") && pageTitle() === name2, 5000)), pageTitle());
    chord("k");
    const pq2 = await until("palette", () => $(".ws-palette input"), 5000);
    setValue(pq2, "src");
    const byFolder = await until("folder hit", () => mainRows().length >= 1 && mainRows().every((r) => text($(".ws-palette-where", r)).includes("src")) && mainRows(), 5000);
    await check("it finds a session by its folder", !!byFolder && byFolder.length === 1, $$(".ws-palette-row").map(text).join(" | "));
    keyDown(pq2, "Enter");
    await check("and opens the Codex one", !!(await until("codex again", () => !$(".ws-palette") && currentKey() === k3 && $(".ws-main .nt-chat.cdx"), 30000)),
      `selected: ${currentKey()}; expected: ${k3}; view: ${$(".ws-main .nt-chat")?.className}`);

    // A9. ↑ / ↓ over the list; a row's button opens it (Enter's own action on a focused button).
    $(".ws-row-main", rowOf(k1))?.focus();
    const from = document.activeElement;
    keyDown(from, "ArrowDown");
    const next = document.activeElement;
    await check("↓ moves to the next item in the list", next !== from && !!next?.closest(".ws-list") && next.hasAttribute("data-ws-nav"), next?.textContent);
    keyDown(next, "ArrowUp");
    await check("↑ moves back", document.activeElement === from);
    document.activeElement.click();
    await check("the focused row opens", !!(await until("opened by key", () => currentKey() === k1, 5000)));

    // E. A fork (SB-29) and the deeper search (SB-06): the row menu's Fork copies k1's conversation into a
    // new session listed under it; the tree folds into a summary line and back; ⌘K finds the fork by what
    // was said in it (not in its parent), and k1 by a title it had before; archiving k1 asks about its fork.
    {
      await openRow(k1);
      await idle(30000);
      const forkKeysBefore = new Set($$(".ws-row[data-row-key]").map((r) => r.dataset.rowKey));
      await rowMenu(k1);
      const forkItem = await until("Fork item", () => byLetter("F"), 3000);
      await check("the row menu offers Fork for an idle Claude session", !!forkItem && !forkItem.disabled && /分叉|Fork/.test(text(forkItem)), $(".ws-menu")?.textContent);
      forkItem?.click();
      const kf = await until("the fork opened", () => {
        const key = currentKey();
        return key && key !== k1 && !forkKeysBefore.has(key) && readyConversation(key) && key;
      }, 30000);
      await check("Fork opens a new session", !!kf, `${currentKey()}`);
      if (!kf) throw sceneError("fork: no new session");
      await check("the fork is told in a toast", $$(".ws-toast").some((x) => /已分叉|Forked/.test(x.textContent)), $$(".ws-toast").map(text).join(" | "));
      const fid = kf.slice(kf.indexOf("/") + 1), pid = k1.slice(k1.indexOf("/") + 1);
      const listed = (await hostCall("chat_sessions")).sessions.find((s) => s.id === fid);
      await check("the host keeps the fork's parent, agent and folder", listed?.parent === pid && listed?.runtime && listed?.cwd === CFG.work,
        JSON.stringify(listed ?? {}).slice(0, 300));
      await check("the fork sits under its parent, indented, with the guide line", rowOf(kf)?.dataset.depth === "1"
        && rowOf(k1)?.dataset.family === "open" && !!$(`.ws-family[data-family-of="${CSS.escape(k1)}"] .ws-row[data-row-key="${CSS.escape(kf)}"]`));
      await check("an unnamed fork is named after its parent", /的分叉|^Fork of /.test(rowTitle(kf)), rowTitle(kf));
      await check("the fork's conversation starts with its parent's turns",
        !!(await until("copied turns", () => $$(".ws-main .nt-user").some((u) => u.textContent.includes(prompt1)), 15000)));
      await send("which word ZEBRAFORK", { newTurn: true });
      await check("the fork's agent continues a copy of the parent's conversation",
        !!(await said("(forked from fake-session)", 30000)), text(lastTurn()).slice(-200));
      await idle();
      await shot("e01-fork-tree");
      // Folding: shown again by the summary line; a second click within 500 ms is ignored.
      await openRow(k1);
      const arrow = () => $(".ws-row-family", rowOf(k1));
      arrow()?.click();
      arrow()?.click();
      const folded = await until("forks hidden", () => rowOf(k1)?.dataset.family === "closed" && !rowOf(kf) && $(".ws-family-sum"), 3000);
      await check("the arrow hides the forks (a second click within 500 ms is ignored) and a line counts them", !!folded
        && /1/.test(text(folded)), text(folded));
      await check("the folded family is remembered here", (localStorage.getItem("ai-remote.ws.families") ?? "").includes(k1));
      await check("the arrow does not open the session", currentKey() === k1);
      await shot("e02-forks-hidden");
      $(".ws-family-sum")?.click();
      await check("the summary line shows the forks again", !!(await until("forks shown", () => rowOf(k1)?.dataset.family === "open" && rowOf(kf), 3000)));
      // ⌘K: what was said in the fork (not in its parent) is found by the hosts' search, with an excerpt.
      chord("k");
      const dq = await until("palette", () => $(".ws-palette input"), 5000);
      setValue(dq, "zebrafork");
      const content = await until("content hit", () => $(`.ws-palette-row[data-content-hit="${CSS.escape(kf)}"]`), 15000);
      await check("⌘K finds the fork by what was said in it, with an excerpt", !!content && /ZEBRAFORK/.test(text(content)) && /在内容中|In conversations/.test(text($("[data-content-head]"))),
        $$(".ws-palette-row").map(text).join(" | "));
      await check("its parent was not told it", !$(`.ws-palette-row[data-content-hit="${CSS.escape(k1)}"]`));
      await shot("e03-deep-search");
      content?.click();
      await check("a content hit opens its session", !!(await until("opened from content", () => !$(".ws-palette") && currentKey() === kf, 10000)), currentKey());
      // A title it had before is a main result, said so.
      const former = "E2E ALPHA 曾用名";
      await hostCall("chat_update", { session: pid, title: former });
      await hostCall("chat_update", { session: pid, title: name1 });
      chord("k");
      const fq = await until("palette", () => $(".ws-palette input"), 5000);
      setValue(fq, "alpha");
      // The list learns the earlier title at its next poll (every 4 s, 20 s while the window is hidden).
      const formerRow = await until("former title listed", () =>
        $$(".ws-palette-row:not([data-content-hit])").find((r) => text($(".ws-palette-title", r)) === name1 && $("[data-formerly]", r)), 30000);
      await check("⌘K finds a session by a title it had before, saying so", !!formerRow && text($("[data-formerly]", formerRow)).includes(former), $$(".ws-palette-row").map(text).join(" | "));
      await shot("e04-formerly");
      keyDown(fq, "Escape");
      await until("palette closed", () => !$(".ws-palette"), 3000);
      // Archiving a session with a fork asks first; "only this one" moves the fork to the top level, Undo puts it back.
      await rowMenu(k1);
      byLetter("A")?.click();
      const ask = await until("archive question", () => $(".ws-confirm"), 5000);
      await check("archiving a session with a fork asks whether the fork goes too", !!ask && /1/.test(text(ask)) && !!$("[data-family-only]", ask) && !!$("[data-family-all]", ask), text(ask));
      await shot("e05-archive-ask");
      $("[data-family-only]", ask)?.click();
      const moved = await until("fork on top", () => !$(".ws-confirm") && !rowOf(k1) && rowOf(kf) && !rowOf(kf).dataset.depth, 10000);
      await check("only this one: the parent is archived and its fork moves to the top level", !!moved);
      const undo = await until("archive toast", () => $$(".ws-toast").find((x) => /已归档|Archived/.test(x.textContent)), 5000);
      $(".ws-toast-act", undo)?.click();
      await check("Undo brings the parent back with its fork under it", !!(await until("family again", () => rowOf(k1) && !rowOf(k1).hasAttribute("data-archived")
        && rowOf(kf)?.dataset.depth === "1", 15000)));
      // The fork's agent ends (the isolated host runs at most four).
      await hostCall("chat_end", { session: fid });
      let forkEnded = false;
      for (const end = Date.now() + 15000; Date.now() < end && !forkEnded; await sleep(250)) {
        forkEnded = (await hostCall("chat_sessions")).sessions.some((s) => s.id === fid && s.state === "ended");
      }
      await check("the fork ends on its host", forkEnded);
      await openRow(k1);
    }

    // A10. ⌘B hides and shows the sidebar; its edge drags to a width that stays; a double click resets it.
    chord("b");
    await check("⌘B hides the sidebar", !!(await until("hidden", () => $(".ws")?.dataset.side === "hidden" && !$(".ws-side"), 3000)));
    await shot("a09-hidden");
    chord("b");
    await check("⌘B shows it again", !!(await until("docked", () => $(".ws")?.dataset.side === "docked" && $(".ws-side"), 3000)));
    {
      const grip = $(".ws-grip");
      const r = grip.getBoundingClientRect();
      const x = r.left + r.width / 2, y = r.top + 300;
      // Synthetic pointers are not real pointers to setPointerCapture: it is stubbed for the drag.
      const capture = Element.prototype.setPointerCapture;
      Element.prototype.setPointerCapture = function () {};
      const ev = (type, dx, buttons) => new PointerEvent(type, { pointerId: 1, pointerType: "mouse", isPrimary: true, button: 0, buttons, clientX: x + dx, clientY: y, bubbles: true, cancelable: true });
      try {
        grip.dispatchEvent(ev("pointerdown", 0, 1));
        for (let i = 1; i <= 6; i++) {
          grip.dispatchEvent(ev("pointermove", i * 10, 1));
          await sleep(30);
        }
        grip.dispatchEvent(ev("pointerup", 60, 0));
      } finally {
        Element.prototype.setPointerCapture = capture;
      }
      await sleep(200);
      const w = Math.round($(".ws-side").getBoundingClientRect().width);
      await check("dragging the edge widens the sidebar and keeps it", w === 348 && JSON.parse(localStorage.getItem("ai-remote.ws.side")).w === 348, `${w} ${localStorage.getItem("ai-remote.ws.side")}`);
      await shot("a10-wider");
      grip.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
      await check("a double click puts it back to 288 px", !!(await until("reset", () => Math.round($(".ws-side").getBoundingClientRect().width) === 288, 3000)));
    }

    // ===== B. The Claude page, "Ask" mode =====
    await newSession();
    await pickAgent("Rich fake");
    await check("the recent folders offer the work folder", !!(await pickRecent(workName)), folderShown());
    await pickMode(0);
    // The approval's first 400 ms: ⌘↵ the moment the card appears must not answer it.
    // Timers are slowed in a hidden window: wait on message-channel turns instead (React's effects, which
    // install the card's keys, run in between), then press ⌘↵ about 150 ms after the card appeared.
    const turn = () => new Promise((r) => { const c = new MessageChannel(); c.port1.onmessage = () => r(); c.port2.postMessage(0); });
    let early = null;
    const watch = new MutationObserver(() => {
      if (early || !$(".ws-main .cl-ap")) return;
      early = { seen: performance.now(), at: null };
      void (async () => {
        while (performance.now() - early.seen < 150) await turn();
        early.at = Math.round(performance.now() - early.seen);
        keyDown(composer() ?? document.body, "Enter", mac ? { metaKey: true } : { ctrlKey: true });
      })();
    });
    watch.observe(document.body, { childList: true, subtree: true });
    const kb = await start("PERMIT 构建一下");
    const ap = await until("approval card", () => $(".ws-main .cl-ap"), 20000);
    watch.disconnect();
    await check("an approval card shows", !!ap && /make build/.test(ap.textContent), ap?.textContent);
    await sleep(700);
    await check("⌘↵ in its first 400 ms does nothing", !!early && early.at !== null && early.at < 400 && !!$(".ws-main .cl-ap") && !lastTurn()?.textContent.includes("Ran it"),
      early ? `pressed at ${early.at} ms` : "the card was never seen appearing");
    const dock = $(".ws-main .nt-compose")?.getBoundingClientRect();
    await check("the card sits right above the composer", !!dock && ap.getBoundingClientRect().bottom <= dock.top + 1 && ap.getBoundingClientRect().bottom > dock.top - 40,
      `${ap.getBoundingClientRect().bottom} / ${dock?.top}`);
    setValue(composer(), "草稿还能打字");
    await check("the composer still takes typing", !composer().disabled && composer().value === "草稿还能打字");
    // The sidebar learns it from its poll (the open conversation's events do not reach the row): measured.
    const amberAt = Date.now();
    const amber = await until("awaiting", () => dotOf(kb) === "awaiting", 30000);
    if (amber) await note(`the amber dot came ${((Date.now() - amberAt) / 1000).toFixed(1)} s after the card`);
    await check("the row's dot turns amber while the approval waits", !!amber, dotOf(kb));
    setValue(composer(), "");
    await shot("b01-approval");
    const btn = (re) => $$(".ws-main .cl-ap .cl-btn").find((b) => re.test(text(b)));
    btn(/允许一次|Allow once/).click();
    await check("Allow once runs the tool", !!(await said("Ran it (once).")) && !$(".ws-main .cl-ap"));
    await idle();
    await send("PERMIT 再来一次");
    await until("approval card", () => $(".ws-main .cl-ap"), 20000);
    await sleep(500);
    const sessionBtn = btn(/本会话内都允许|Allow for this session/);
    await check("the card offers Allow for this session", !!sessionBtn);
    sessionBtn?.click();
    await check("Allow for this session hands the agent the session rule", !!(await said("Ran it (for this session).")));
    await idle();
    await send("PERMIT 第三次");
    await until("approval card", () => $(".ws-main .cl-ap"), 20000);
    await sleep(500);
    const noteBtn = btn(/拒绝并说明|Deny with a note/);
    await check("the approval card can deny with a note for the agent", !!noteBtn, $(".ws-main .cl-ap .cl-btns")?.textContent);
    noteBtn?.click();
    const noteField = await until("note field", () => $(".ws-main .cl-ap textarea"), 3000);
    if (noteField) {
      setValue(noteField, "别动那个目录");
      await sleep(200);
      btn(/拒绝并发送|Deny and send/)?.click();
    } else btn(/^拒绝|^Deny/)?.click();
    await check("Deny with a note tells the agent the note", !!(await until("denied", () => /Not done: 别动那个目录/.test(lastTurn()?.textContent ?? ""), 15000)), lastTurn()?.textContent.slice(-120));
    await shot("b02b-deny-note");
    await idle();
    await send("PERMIT 第四次");
    await until("approval card", () => $(".ws-main .cl-ap"), 20000);
    await sleep(500);
    btn(/^(拒绝|Deny)\s*(esc)?$/)?.click();
    await check("Deny tells the agent no", !!(await until("denied", () => /Not done: (?:The user )?declined/.test(lastTurn()?.textContent ?? ""), 15000)), lastTurn()?.textContent.slice(-120));
    const deniedTool = $(".cl-tool[data-state='denied']", lastTurn());
    await check("a denied tool is not labelled as executed", !!deniedTool && !/运行了|^Ran\b/.test(text($(".cl-lbl", deniedTool))), text(deniedTool));
    await idle();

    // B2. A question in two parts: single choice, then multiple with Other; the answers in the transcript.
    await send("ASK 两个问题");
    const qc = await until("question card", () => $(".ws-main .cl-qc"), 20000);
    await check("a question card shows, page 1 of 2", !!qc && /1\s*(\/|of)\s*2/.test(text($(".pager .tnum", qc))), text($(".pager", qc)));
    await check("the row's dot turns amber (waiting for you)", !!(await until("awaiting", () => dotOf(kb) === "awaiting", 15000)), dotOf(kb));
    await sleep(500);
    const opt = (label) => $$(".ws-main .cl-qc-opts .opt").find((o) => text($(".nm", o)) === label);
    opt("Q3").click();
    await shot("b02-question");
    $(".ws-main .cl-qc-ft .cl-btn.primary").click();
    await check("Next goes to the second part", !!(await until("page 2", () => /2\s*(\/|of)\s*2/.test(text($(".ws-main .cl-qc .pager .tnum"))), 3000)));
    opt("CSV").click();
    $$(".ws-main .cl-qc-opts .opt").at(-1).click();
    const other = await until("other field", () => $(".ws-main .cl-qc-other"), 3000);
    if (other) setValue(other, "ODS");
    await check("two picks on the multiple-choice part", $$(".ws-main .cl-qc-opts .opt[aria-checked=true]").length === 2);
    await shot("b03-question-other");
    $(".ws-main .cl-qc-ft .cl-btn.primary").click();
    await check("submitting closes the card", !!(await until("card gone", () => !$(".ws-main .cl-qc"), 10000)));
    const receipt = await until("receipt", () => $(".ws-main .cl-receipt"), 10000);
    await check("the transcript shows both answers", !!receipt && /Q3/.test(receipt.textContent) && /CSV/.test(receipt.textContent) && /ODS/.test(receipt.textContent), receipt?.textContent);
    await check("the agent got them", !!(await said("You picked:")) && /Q3/.test(lastTurn().textContent) && /ODS/.test(lastTurn().textContent), lastTurn()?.textContent.slice(-160));
    const answeredAt = Date.now();
    const amberGone = await until("not awaiting", () => dotOf(kb) !== "awaiting", 30000);
    if (amberGone) await note(`the amber dot went ${((Date.now() - answeredAt) / 1000).toFixed(1)} s after the answer (the sidebar's poll)`);
    let hostPending = "";
    if (!amberGone) {
      const l = await hostCall("chat_sessions").catch(() => ({}));
      const m = (l.sessions ?? []).find((x) => kb.endsWith(`/${x.id}`));
      hostPending = `host: pending=${m?.pending ?? "none"} state=${m?.state}`;
    }
    await check("the amber dot goes", !!amberGone, `${dotOf(kb)}; ${hostPending}`);
    await idle();

    // B3. A plan sent back with a note, then one accepted into "Accept edits" (the host's mode event).
    await send("PLAN 先出计划");
    const pl = await until("plan card", () => $(".ws-main .cl-pl"), 20000);
    await check("a plan card shows the plan", !!pl && /month's real last day/.test(pl.textContent), pl?.textContent.slice(0, 120));
    await shot("b04-plan");
    await sleep(500);
    $$(".ws-main .cl-pl .cl-btns .l .cl-btn")[1].click();
    const pn = await until("plan note", () => $(".ws-main .cl-pl-note"), 3000);
    if (pn) setValue(pn, "先别改 CI");
    $(".ws-main .cl-pl .cl-btns .r .cl-btn.primary").click();
    await check("Revise sends the note back: the agent reads it", !!(await said("Not done: 先别改 CI")));
    await idle();
    await send("PLAN 再出一次");
    await until("plan card", () => $(".ws-main .cl-pl"), 20000);
    await sleep(500);
    const modeChip = () => $(".ws-main .cl-mode .cl-chip")?.getAttribute("aria-label") ?? "";
    {
      const l = await hostCall("chat_sessions").catch(() => ({}));
      const m = (l.sessions ?? []).find((x) => kb.endsWith(`/${x.id}`));
      await check("the mode under the composer reads Ask before edits", /编辑前询问|Ask before edits/.test(modeChip()), `${modeChip()}; host permission_mode=${m?.permission_mode}`);
    }
    $(".ws-main .cl-pl .cl-split .cl-btn.primary:not(.more)").click();
    await check("accepting closes the card", !!(await until("plan gone", () => !$(".ws-main .cl-pl"), 5000)));
    await check("the host's mode event moves the chip to Accept edits", !!(await until("mode acceptEdits", () => /自动接受编辑|Accept edits/.test(modeChip()), 10000)), modeChip());
    await idle();

    // B4. The mode menu: plan; the chip follows the host.
    $(".ws-main .cl-mode .cl-chip").click();
    const modes = await until("mode menu", () => $$(".ws-main .cl-mode .cl-menu [role=menuitemradio]").length === 4 && $$(".ws-main .cl-mode .cl-menu [role=menuitemradio]"), 3000);
    await shot("b05-mode-menu");
    modes?.[2].click();
    await check("choosing Plan mode comes back from the host", !!(await until("mode plan", () => /计划模式|Plan mode/.test(modeChip()), 10000)), modeChip());

    // B5. An edit's diff with its real line numbers.
    await send("EDITAT 改第 22 行");
    await said("Edited export.py at lines 22 and 40.");
    await idle();
    const grp = $(".cl-group", lastTurn());
    if (grp && !$(".cl-group-bd", grp)) $(".cl-group-hd", grp).click();
    const editRow = await until("edit row", () => $$(".cl-tool", lastTurn()).find((x) => /Edited|编辑了|export\.py/.test(x.textContent)), 5000);
    if (editRow && !$(".cl-diff-rows", editRow)) $(".cl-tool-hd", editRow).click();
    const rows = await until("numbered diff", () => $(".cl-diff-rows.numbered", lastTurn()), 5000);
    const nums = rows ? $$(".ln", rows).map((l) => $$(".no", l).map(text).join("/")) : [];
    await check("the diff shows the hunks' real line numbers", nums[0] === "22/22" && nums.some((n) => n.startsWith("40/")) && !!$(".gap", rows), nums.join(" "));
    await check("the edit shows its change once", !!editRow && $$(".cl-diff", editRow).length === 1, `${editRow ? $$(".cl-diff", editRow).length : 0} diffs`);
    await shot("b06-diff");

    // B6. Stop, then continue.
    await send("SLOW 慢任务");
    await until("running", running, 10000);
    $(".ws-main .nt-compose .round.stop").click();
    const stopped = await until("stopped", () => $(".cl-stopped", lastTurn()), 10000);
    await check("Stop ends the turn and offers Continue", !!stopped);
    await check("b07-stopped: the tool reaches stopped after its specified display dwell",
      !!(await until("displayed tool stopped", () => $('.cl-tool[data-state="stopped"]', lastTurn()), 3000)));
    await shot("b07-stopped");
    $(".cl-stopped .cl-btn", lastTurn())?.click();
    await check("Continue sends a new turn", !!(await until("continued", () => !$(".ws-main .cl-stopped") && /you said: (从刚才|Continue where)/.test(lastTurn()?.textContent ?? ""), 15000)), lastTurn()?.textContent.slice(-100));
    await idle();

    // ===== C. First answer wins: another connection answers the approval =====
    const other2 = { events: [] };
    try {
      const att = await hostCall("chat_attach", { session: kb.split("/").pop() });
      const cb = window.__TAURI_INTERNALS__.transformCallback((raw) => { if (raw && "message" in raw) other2.events.push(raw.message); });
      const channel = { toJSON: () => `__CHANNEL__:${cb}`, __TAURI_TO_IPC_KEY__: () => `__CHANNEL__:${cb}` };
      const [id, hello] = await invoke("open_chat", { host: CFG.host, request: { ticket: att.ticket, since: 0 }, onEvent: channel });
      other2.id = id;
      // Replayed approvals precede this hello; only answer the new request that this step sends.
      other2.since = hello.seq;
    } catch (e) {
      other2.error = String(e && e.message || e);
    }
    await check("a second connection opens to the same session", other2.id !== undefined, other2.error);
    if (other2.id !== undefined) {
      await send("PERMIT 另一端来批");
      const card = await until("approval card", () => $(".ws-main .cl-ap"), 20000);
      const ev = await until("permission event on the second connection", () => other2.events.filter((e) => e && e.t === "permission" && e.seq > other2.since).at(-1), 10000);
      await check("the card shows here and the request on the other connection", !!card && !!ev, ev?.rid);
      await shot("c01-before-other");
      if (ev) await invoke("chat_send", { id: other2.id, command: { permit: { rid: ev.rid, decision: "once" } } });
      await check("the other connection's answer closes the card here", !!(await until("card gone", () => !$(".ws-main .cl-ap"), 10000)));
      await check("and the tool runs once", !!(await said("Ran it (once).")));
      const modes = other2.events.filter((e) => e && e.t === "mode").map((e) => `${e.seq}:${e.mode}`);
      note(`mode events in the session's log: ${modes.join(", ") || "none"}`);
      const res = other2.events.find((e) => e && e.t === "resolved" && e.id === ev?.rid);
      await check("the host reports it resolved (who answered)", !!res, JSON.stringify(res ?? {}).slice(0, 160));
      if (!/另一|other device|elsewhere|已在/.test(text(lastTurn()))) note("nothing on the page says another device answered (the card just closes)");
      await check("c02-after-other: the tool reaches done after its specified display dwell",
      !!(await until("displayed tool done", () => $('.cl-tool[data-state="done"]', lastTurn()), 3000)));
    await shot("c02-after-other");
      const otherAt = Date.now();
      const settled = await until("not awaiting", () => dotOf(kb) !== "awaiting", 30000);
      if (settled) await note(`the amber dot went ${((Date.now() - otherAt) / 1000).toFixed(1)} s after the other connection answered`);
      let why = "";
      if (!settled) {
        const l = await hostCall("chat_sessions").catch(() => ({}));
        const m = (l.sessions ?? []).find((x) => kb.endsWith(`/${x.id}`));
        why = `host: pending=${m?.pending ?? "none"} state=${m?.state}`;
      }
      await check("the amber dot goes once the other connection answered", !!settled, `${dotOf(kb)}; ${why}`);
      await invoke("close_chat", { id: other2.id }).catch(() => {});
      await idle();
    }

    // B7. A waiting message (an agent that takes no message mid-turn): edited, then one taken back. The host
    // runs four agents at most: the Codex session is ended first (its menu's End, confirmed).
    await rowMenu(k3);
    const endItem = () => $$(".ws-menu .ws-menu-item.danger")[0];
    endItem()?.click();
    await check("End asks to confirm in place", !!(await until("confirm", () => /再点一次|Click again/.test(text(endItem())), 3000)), text(endItem()));
    endItem()?.click();
    let ended = false;
    for (let i = 0; i < 20 && !ended; i++) {
      await sleep(500);
      const l = await hostCall("chat_sessions").catch(() => ({}));
      ended = (l.sessions ?? []).find((x) => k3.endsWith(`/${x.id}`))?.state === "ended";
    }
    closeMenu();
    await check("confirming ends the session on the host", ended);
    await newSession();
    await pickAgent("No-steer fake");
    await pickRecent(workName);
    const kq = await start("SLOW 排队测试");
    await check("the no-steer session runs", !!kq && !!(await until("running", running, 15000)));
    await send("排队一条");
    const qd = await until("queued", () => $$(".ws-main .nt-qd").find((q) => q.textContent.includes("排队一条")), 5000);
    await check("a message sent mid-turn waits above the composer", !!qd);
    $$(".nt-ib", qd).find((b) => /编辑|Edit/.test(b.getAttribute("aria-label") ?? ""))?.click();
    const qe = await until("editing", () => $(".ws-main .nt-qd.editing textarea"), 3000);
    if (qe) {
      setValue(qe, "排队一条（已改）");
      keyDown(qe, "Enter");
    }
    await check("editing it replaces the waiting message", !!(await until("edited", () => $$(".ws-main .nt-qd").some((q) => q.textContent.includes("排队一条（已改）")) && !$(".ws-main .nt-qd.editing"), 5000)),
      $$(".ws-main .nt-qd").map(text).join(" | "));
    await send("撤回我");
    const qd2 = await until("second queued", () => $$(".ws-main .nt-qd").find((q) => q.textContent.includes("撤回我")), 5000);
    await shot("b08-queued");
    $$(".nt-ib", qd2).at(-1)?.click();
    await check("a waiting message can be taken back", !!(await until("taken back", () => !$$(".ws-main .nt-qd").some((q) => q.textContent.includes("撤回我")), 5000)));
    $(".ws-main .nt-compose .round.stop").click();
    await check("after Stop the edited message runs next", !!(await said("you said: 排队一条（已改）", 20000)));
    await idle();
    await check("the one taken back never ran", !$$(".ws-main .nt-user").some((u) => u.textContent.includes("撤回我")));
    await shot("b09-after-queue");

    // The current conversation already shares the live screen. The same computer's conversations,
    // projects and history change only the right side of the window.
    const rail = await until("this conversation beside the live view", () => {
      const el = $(".nt-live-workspace .ws[data-embedded=true] .nt-chat");
      return el && el.textContent.includes("SLOW 排队测试") && el.textContent.includes("you said: 排队一条（已改）") && el;
    }, 30000);
    await check("the current conversation is still beside the live view", !!rail && rail.classList.contains("cl-root"));
    await check("the live view has no old steps or agent-perspective tabs", !$(".nt-rail > .seg"));
    await shot("c03-watch-current-session");

    // ===== D. Batch actions use the mounted UI and the real host's stored metadata =====
    // Keep the current conversation out of the batch: opening it would automatically mark it read.
    // Reuse A's k1/k2 and the already-ended Codex k3; this section starts no additional agent.
    const batchKeys = [k1, k2, k3];
    const batchIds = batchKeys.map((key) => key.slice(key.indexOf("/") + 1));
    const selectedKeys = () => $$(".ws-row[data-multi-selected]").map((row) => row.dataset.rowKey);
    const sameKeys = (actual, expected) => actual.length === expected.length && expected.every((key) => actual.includes(key));
    const pinnedRow = (key) => rowOf(key)?.closest(".ws-sec")?.dataset.group === "pinned";
    const batchSummary = (el, undo = false) => undo
      ? /已撤销 3 个，未撤销 0 个|Undid 3; could not undo 0/.test(text(el))
      : /已完成 3 个，失败 0 个|Completed 3; failed 0/.test(text(el));
    const batchHost = async (label, matches, args = {}) => {
      const end = Date.now() + 10000;
      let last = null, error = "";
      do {
        try {
          last = await hostCall("chat_sessions", args);
          if (matches(last.sessions ?? [])) return last;
        } catch (e) { error = String(e && e.message || e); }
        await sleep(250);
      } while (Date.now() < end);
      await note(`batch host timeout ${label}: ${error || JSON.stringify(last).slice(0, 300)}`);
      return null;
    };
    const clearBatch = async () => {
      keyDown(document.activeElement ?? document.body, "Escape");
      if (!await until("batch selection cleared", () => !selectedKeys().length && !$(".ws-menu"), 3000)) {
        throw sceneError("batch: selection did not clear");
      }
    };
    const selectBatchRow = async (key, modifiers) => {
      const button = $(".ws-row-main", rowOf(key));
      if (!button?.isConnected) throw sceneError("batch: a target row disappeared", { key });
      button.focus({ preventScroll: true });
      button.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, ...modifiers }));
      await sleep(100);
    };
    const selectBatch = async () => {
      await clearBatch();
      for (const key of batchKeys) await selectBatchRow(key, { [mac ? "metaKey" : "ctrlKey"]: true });
      if (!sameKeys(selectedKeys(), batchKeys)) throw sceneError("batch: wrong selected sessions", { selected: selectedKeys(), batchKeys });
    };
    const batchMenu = async () => {
      const row = rowOf(k1);
      if (!row?.isConnected) throw sceneError("batch: context row is missing");
      row.scrollIntoView({ block: "nearest" });
      const rect = row.getBoundingClientRect();
      row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2, clientX: rect.left + 24, clientY: rect.top + rect.height / 2 }));
      const menu = await until("batch menu", () => $(".ws-batch-menu"), 3000);
      if (!menu) throw sceneError("batch: required menu did not open", { selected: selectedKeys() });
      return menu;
    };
    const batchAction = async (letter, label) => {
      const menu = $(".ws-batch-menu");
      const button = byLetter(letter);
      if (!menu?.isConnected || !button?.isConnected || button.disabled || !label.test(text(button))) {
        throw sceneError("batch: required action is unavailable", { letter, expected: String(label), menu: text(menu) });
      }
      if (!menu.contains(document.activeElement)) throw sceneError("batch: menu did not receive keyboard focus", { letter });
      const previous = new Set($$(".ws-toast"));
      keyDown(document.activeElement, letter.toLowerCase(), { code: `Key${letter}` });
      const toast = await until("batch result", () => $$(".ws-toast").find((el) => !previous.has(el) && batchSummary(el)), 10000);
      if (!toast) throw sceneError("batch: missing three-success result", { letter, toasts: $$(".ws-toast").map(text) });
      return toast;
    };
    const undoBatchToast = async (toast) => {
      const button = $(".ws-toast-act", toast);
      if (!button?.isConnected) throw sceneError("batch: Undo disappeared before use");
      const previous = new Set($$(".ws-toast"));
      button.click();
      const undone = await until("batch undone", () => $$(".ws-toast").find((el) => !previous.has(el) && batchSummary(el, true)), 10000);
      if (!undone) throw sceneError("batch: missing three-success Undo result", { toasts: $$(".ws-toast").map(text) });
      return undone;
    };
    const baseline = await batchHost("three idle non-current sessions", (xs) => batchIds.every((id) => {
      const meta = xs.find((x) => x.id === id);
      return meta && !["running", "starting", "interrupting"].includes(meta.state) && !meta.pending;
    }));
    await check("batch targets three distinct idle sessions outside the current conversation", !!baseline && new Set(batchKeys).size === 3
      && !batchKeys.includes(currentKey()) && batchKeys.every((key) => rowOf(key)), JSON.stringify({ batchKeys, current: currentKey() }));
    if (!baseline || new Set(batchKeys).size !== 3 || batchKeys.includes(currentKey()) || !batchKeys.every((key) => rowOf(key))) {
      throw sceneError("batch: preconditions failed", { batchKeys });
    }
    const beforeBatch = new Map(baseline.sessions.filter((x) => batchIds.includes(x.id)).map((meta) => [meta.id, meta]));
    const flagsMatch = (xs) => batchIds.every((id) => {
      const meta = xs.find((x) => x.id === id), before = beforeBatch.get(id);
      return meta && !!meta.pinned === !!before.pinned && !!meta.archived === !!before.archived && meta.title === before.title;
    });
    const checkBatchOwnership = async () => {
      const pane = $('.nt-live-workspace .ws[data-embedded="true"]');
      const aside = pane?.closest("aside.nt-workspace-pane");
      const screen = $(".nt-live-workspace .nt-stage");
      const pair = [k1, k2], watchKeys = [...batchKeys, kq];
      if (!pane || !aside || !screen || aside.hasAttribute("inert")) throw sceneError("batch ownership: live panes are unavailable");
      const metadata = () => JSON.stringify(watchKeys.map((key) => ({
        key, title: rowTitle(key), pinned: pinnedRow(key), archived: rowOf(key)?.hasAttribute("data-archived"),
      })));
      // Status dots follow independently refreshed turn/read state, not the host's editable metadata.
      const dots = () => JSON.stringify(watchKeys.map((key) => dotOf(key)));
      // idle() observes the conversation footer; the independently polled list may still show the
      // first-message title. Start the key probes only after this host snapshot is drawn by the UI.
      // A hidden workspace legitimately polls every 20 s. This is a derived readiness budget,
      // not measured title latency; the independent decoded-frame check remains 30 s.
      const ownershipDeadline = Date.now() + (document.hidden ? 25000 : 10000);
      const ownershipBaseline = await batchHost("ownership metadata ready", (xs) => watchKeys.every((key) => {
        const id = key.slice(key.indexOf("/") + 1), meta = xs.find((x) => x.id === id);
        return meta && (key !== kq || (typeof meta.title === "string" && !!meta.title.trim()));
      }));
      const shownHostTitle = (meta) => {
        const title = typeof meta.title === "string" ? meta.title.trim() : "";
        if (title) return title;
        const first = (meta.first_text ?? "").split(/\r?\n/).map((line) => line.trim()).find(Boolean) ?? "";
        const firstLine = first.length > 80 ? `${first.slice(0, 79).trimEnd()}…` : first;
        return firstLine || (meta.name ?? "").trim();
      };
      const remaining = ownershipDeadline - Date.now();
      const ready = ownershipBaseline && remaining > 0 && await until("ownership host metadata drawn in the list", () =>
        watchKeys.every((key) => {
          const id = key.slice(key.indexOf("/") + 1), meta = ownershipBaseline.sessions.find((x) => x.id === id), row = rowOf(key);
          return meta && row?.isConnected && rowTitle(key) === shownHostTitle(meta)
            && pinnedRow(key) === !!meta.pinned && row.hasAttribute("data-archived") === !!meta.archived;
        }), remaining);
      if (!ready) throw sceneError("batch ownership: host metadata did not reach the list within the shared readiness budget");
      const before = metadata(), beforeDots = dots();
      const checkpoints = [];
      let hostMetadataUnchanged = false;
      const shortcuts = [
        { key: "F2", code: "F2" }, { key: "Escape", code: "Escape" },
        ...["metaKey", "ctrlKey"].flatMap((modifier) => ["P", "U", "A", "R"].map((letter) =>
          ({ key: letter.toLowerCase(), code: `Key${letter}`, altKey: true, [modifier]: true }))),
      ];
      const probe = (target, keys) => keys.map((options) => {
        const event = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...options });
        let targetReached = false, documentReached = false;
        const atTarget = (e) => { if (e === event) targetReached = true; };
        const atDocument = (e) => { if (e === event) documentReached = true; };
        target.addEventListener("keydown", atTarget);
        document.addEventListener("keydown", atDocument);
        try {
          target.dispatchEvent(event);
          return { ...options, prevented: event.defaultPrevented, targetReached, documentReached };
        } finally {
          target.removeEventListener("keydown", atTarget);
          document.removeEventListener("keydown", atDocument);
        }
      });
      const allowed = (rows) => rows.every((x) => !x.prevented && x.targetReached && x.documentReached);
      const blocked = (rows) => rows.every((x) => x.prevented && !x.targetReached && !x.documentReached);
      const originalTabIndex = screen.getAttribute("tabindex"), originalInert = aside.getAttribute("inert");
      const checkpoint = (phase, flowOk, selectionOk, focusOk) => {
        const result = {
          phase, flow_ok: flowOk, selection_ok: selectionOk, current_ok: currentKey() === kq,
          metadata_ok: metadata() === before, dots_unchanged: dots() === beforeDots,
          menu_closed: !$(".ws-menu"), focus_ok: focusOk,
          inert_ok: phase === "dom-inert" ? aside.hasAttribute("inert") : aside.getAttribute("inert") === originalInert,
        };
        checkpoints.push(result);
        return result;
      };
      try {
        // Focus the stage itself, not RemoteInput: these synthetic probes must never send host keys.
        // No pointerdown is dispatched, because a real unmodified outside pointer clears selection.
        screen.setAttribute("tabindex", "-1");
        screen.focus({ preventScroll: true });
        const screenFocusOk = document.activeElement === screen && pane.dataset.keyboardActive === "false" && sameKeys(selectedKeys(), pair);
        const left = probe(screen, shortcuts);
        await check("screen focus keeps the two-row batch selected and releases workspace keyboard ownership",
          screenFocusOk);
        await note(`batch-keyboard-ownership ${JSON.stringify({ phase: "screen", probes: left })}`);
        const screenResult = checkpoint("screen", allowed(left), sameKeys(selectedKeys(), pair),
          document.activeElement === screen && pane.dataset.keyboardActive === "false");
        await check("the screen's F2, Escape and both modifier shortcut sets pass the workspace capture handlers", screenResult.flow_ok
          && screenResult.selection_ok && screenResult.current_ok && screenResult.metadata_ok && screenResult.menu_closed);
        try {
          // Offscreen-safe DOM boundary probe for the inert pane used by immersive mode. This does
          // not enter OS full screen or claim that physical immersive keyboard input was exercised.
          aside.setAttribute("inert", "");
          const inert = probe(pane, shortcuts);
          await note(`batch-keyboard-ownership ${JSON.stringify({ phase: "dom-inert", probes: inert })}`);
          const inertResult = checkpoint("dom-inert", allowed(inert), sameKeys(selectedKeys(), pair),
            document.activeElement === screen && pane.dataset.keyboardActive === "false");
          await check("an inert workspace does not capture F2, Escape or either modifier shortcut set", inertResult.flow_ok
            && inertResult.selection_ok && inertResult.current_ok && inertResult.metadata_ok && inertResult.menu_closed);
        } finally {
          if (originalInert === null) aside.removeAttribute("inert");
          else aside.setAttribute("inert", originalInert);
        }
        const rowButton = $(".ws-row-main", rowOf(k1));
        rowButton.focus({ preventScroll: true });
        const owned = probe(rowButton, shortcuts.filter((x) => x.key !== "Escape"));
        const restoredResult = checkpoint("restored", blocked(owned), sameKeys(selectedKeys(), pair),
          pane.dataset.keyboardActive === "true");
        await check("restoring workspace ownership blocks single-row shortcuts while two rows remain selected",
          restoredResult.inert_ok && restoredResult.focus_ok && restoredResult.flow_ok
            && restoredResult.selection_ok && restoredResult.metadata_ok && restoredResult.menu_closed);
        const escape = probe(rowButton, [shortcuts[1]]);
        const escapeFlowOk = blocked(escape);
        const escapeSelectionOk = escapeFlowOk
          ? !!(await until("owned batch Escape clears selection", () => !selectedKeys().length, 3000))
          : !selectedKeys().length;
        const escapedResult = checkpoint("escaped", escapeFlowOk, !!escapeSelectionOk, pane.dataset.keyboardActive === "true");
        await check("Escape owned by the workspace clears the batch and is consumed", escapedResult.flow_ok && escapedResult.selection_ok);
        // Always query the host, even when the DOM metadata comparison fails, to distinguish the two.
        hostMetadataUnchanged = !!(await batchHost("ownership probes did not mutate session metadata", (xs) =>
          watchKeys.every((key) => {
            const id = key.slice(key.indexOf("/") + 1), meta = xs.find((x) => x.id === id), original = ownershipBaseline.sessions.find((x) => x.id === id);
            return meta && original && !!meta.pinned === !!original.pinned && !!meta.archived === !!original.archived && meta.title === original.title;
          })));
        // This final phase also records the DOM half of the metadata check after the host query.
        escapedResult.metadata_ok = metadata() === before;
        await check("ownership key probes leave every test session's pin, archive and title metadata unchanged",
          escapedResult.metadata_ok && hostMetadataUnchanged);
      } finally {
        if (originalInert === null) aside.removeAttribute("inert");
        else aside.setAttribute("inert", originalInert);
        if (originalTabIndex === null) screen.removeAttribute("tabindex");
        else screen.setAttribute("tabindex", originalTabIndex);
        await say("e2e", `     (workspace ownership diagnostic ${JSON.stringify({ schema: 1, checkpoints, host_metadata_unchanged: hostMetadataUnchanged })})`);
      }
    };
    await check("batch starts with all three unpinned and unarchived", [...beforeBatch.values()].every((meta) => !meta.pinned && !meta.archived));
    if ([...beforeBatch.values()].some((meta) => meta.pinned || meta.archived)) throw sceneError("batch: unexpected original flags");
    await clearBatch();
    await selectBatchRow(k1, { metaKey: true });
    await check("Cmd-click selects without opening the session", sameKeys(selectedKeys(), [k1]) && currentKey() === kq, selectedKeys().join(", "));
    await selectBatchRow(k2, { ctrlKey: true });
    await check("Ctrl-click adds another session", sameKeys(selectedKeys(), [k1, k2]) && currentKey() === kq, selectedKeys().join(", "));
    await checkBatchOwnership();
    await selectBatchRow(k1, { metaKey: true });
    await selectBatchRow(k2, { ctrlKey: true });
    await selectBatchRow(k1, { metaKey: true });
    await check("another modifier click toggles the row off", sameKeys(selectedKeys(), [k2]), selectedKeys().join(", "));
    await clearBatch();
    const drawn = $$(".ws-row[data-row-key]").map((row) => row.dataset.rowKey);
    const rangeStart = Math.min(drawn.indexOf(k1), drawn.indexOf(k2)), rangeEnd = Math.max(drawn.indexOf(k1), drawn.indexOf(k2));
    await selectBatchRow(drawn[rangeStart], { ctrlKey: true });
    await selectBatchRow(drawn[rangeEnd], { shiftKey: true });
    await check("Shift-click selects the range in the drawn list order", sameKeys(selectedKeys(), drawn.slice(rangeStart, rangeEnd + 1)), selectedKeys().join(", "));
    await selectBatch();
    const bulkMenu = await batchMenu();
    await check("right-clicking a selected row opens the three-session batch menu (pin, read, Move to group, archive)", /3/.test(bulkMenu.getAttribute("aria-label") ?? "")
      && $$(".ws-menu-item", bulkMenu).length === 4 && /移动到组|Move 3 sessions to group/.test(text(bulkMenu)));
    await check("the batch menu receives keyboard focus", bulkMenu.contains(document.activeElement), text(document.activeElement));
    // Resource evidence from WK itself: fonts.check can succeed through fallback. The shared
    // five-second deadline is only a waiting budget, not a measured font-loading duration.
    const fontBudgetMs = 5000;
    let fontTimer, fontsReady = false, fontEvidence;
    try {
      fontEvidence = await Promise.race([
        (async () => {
          await document.fonts.ready;
          fontsReady = true;
          const faces = await document.fonts.load('12px "Geist Mono"');
          return { faces: faces.map((face) => ({ family: face.family, status: face.status })) };
        })(),
        new Promise((_, reject) => { fontTimer = setTimeout(() => reject(new Error("font resource check exceeded its waiting budget")), fontBudgetMs); }),
      ]);
    } catch (e) {
      fontEvidence = { faces: [], error: String(e && e.message || e) };
    } finally {
      clearTimeout(fontTimer);
    }
    const geistFaces = fontEvidence.faces.filter((face) => face.family.replace(/["']/g, "").trim() === "Geist Mono");
    const fontReport = { budget_ms: fontBudgetMs, ready: fontsReady, ...fontEvidence };
    await note(`native webview bundled font resource: ${JSON.stringify({ engine: mac ? "WKWebView" : "WebView2", ...fontReport })}`);
    await check("the native webview loads the bundled Geist Mono FontFace before the batch screenshots", fontsReady && !fontEvidence.error
      && geistFaces.length > 0 && geistFaces.every((face) => face.status === "loaded"),
      JSON.stringify(fontReport));
    // This proves that WK can load the bundled resource, not which font exclusively paints a glyph.
    await shot("d01-batch-menu");
    const pinToast = await batchAction("P", /^置顶 3 个会话|^Pin 3 sessions/);
    await check("batch Pin moves all three rows into Pinned", !!(await until("three pinned rows", () => batchKeys.every(pinnedRow), 5000)));
    const hostPinned = await batchHost("three pins persisted", (xs) => batchIds.every((id) => xs.some((x) => x.id === id && x.pinned === true)));
    await check("the real host stores all three batch pins", !!hostPinned);
    await undoBatchToast(pinToast);
    await check("pin Undo restores all three rows' original positions", !!(await until("three unpinned rows", () => batchKeys.every((key) => rowOf(key) && !pinnedRow(key)), 5000)));
    await check("the real host restores only the batch pin fields and retains titles", !!(await batchHost("original pin flags", flagsMatch)));

    // Normalize pre-existing unread marks through the same menu, then verify both U directions.
    if (batchKeys.some((key) => dotOf(key) === "unread")) {
      await selectBatch(); await batchMenu();
      await batchAction("U", /标为已读|as read/);
    }
    await check("all three non-current rows start the unread check without a blue dot", !!(await until("initial read marks", () => batchKeys.every((key) => dotOf(key) !== "unread"), 5000)));
    await selectBatch(); await batchMenu();
    await batchAction("U", /标为未读|as unread/);
    await check("batch Unread gives all three non-current rows a blue dot", !!(await until("three unread dots", () => batchKeys.every((key) => dotOf(key) === "unread"), 5000)));
    await check("marking the batch unread keeps the current conversation", currentKey() === kq);
    await selectBatch(); await batchMenu();
    await batchAction("U", /标为已读|as read/);
    await check("batch Read clears all three blue dots", !!(await until("three read rows", () => batchKeys.every((key) => dotOf(key) === null), 5000)));

    await selectBatch(); await batchMenu();
    const archiveToast = await batchAction("A", /^归档 3 个会话|^Archive 3 sessions/);
    await check("batch Archive removes the three rows from the active list", !!(await until("batch rows archived", () => batchKeys.every((key) => !rowOf(key)), 5000)));
    await check("the real host's archived query contains the three archived sessions", !!(await batchHost("three archived flags", (xs) => batchIds.every((id) => xs.some((x) => x.id === id && x.archived === true)), { archived: true })));
    await check("the real host's active query omits all three archived sessions", !!(await batchHost("archived absent from active", (xs) => batchIds.every((id) => !xs.some((x) => x.id === id)))));
    await undoBatchToast(archiveToast);
    await check("archive Undo restores all three active rows", !!(await until("batch rows restored", () => batchKeys.every((key) => rowOf(key) && !rowOf(key).hasAttribute("data-archived")), 5000)));
    await check("the real host restores the original archive and pin flags without losing titles", !!(await batchHost("original archived flags", flagsMatch)));
    await check("the real host's archived query no longer contains these three sessions", !!(await batchHost("archive Undo persisted", (xs) => batchIds.every((id) => !xs.some((x) => x.id === id)), { archived: true })));
    await check("batch actions finish with no selection and the same current conversation", !selectedKeys().length && !$(".ws-menu") && currentKey() === kq);
    await shot("d02-batch-restored");

    // ===== E. Groups made here, drag, density and motion (SB-04 / SB-08 / SB-15 / SB-28 / SB-36 / SB-42 / VS-34) =====
    // Driven by DOM events (pointer events for the drags, keydown for the keyboard paths). Groups live in
    // this device's storage; pins still go through chat_update and are confirmed on the real host.
    const groupsKey = "ai-remote.ws.groups";
    const groupsSaved = () => { try { return JSON.parse(localStorage.getItem(groupsKey) ?? "null") ?? { groups: [], members: {}, pinOrder: [] }; } catch { return { groups: [], members: {}, pinOrder: [] }; } };
    const listPrefs = () => { try { return JSON.parse(localStorage.getItem("ai-remote.ws.list") ?? "{}"); } catch { return {}; } };
    const groupId = (name) => groupsSaved().groups.find((g) => g.name === name)?.id ?? null;
    const groupSec = (name) => { const id = groupId(name); return id ? $(`[data-custom-group="${CSS.escape(id)}"]`) : null; };
    const inGroup = (key, name) => !!groupSec(name)?.contains(rowOf(key));
    const inPinned = (key) => !!rowOf(key)?.closest("[data-pin-zone]");
    const ungrouped = () => $("[data-ungrouped]");
    const menuLabel = (re) => $$(".ws-menu .ws-menu-item").find((b) => re.test(text($(".ws-menu-label", b))));
    const center = (el) => { const r = el.getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; };
    const ptr = (type, x, y) => (document.elementFromPoint(x, y) ?? document.body).dispatchEvent(new PointerEvent(type, {
      bubbles: true, cancelable: true, composed: true, pointerId: 11, pointerType: "mouse", isPrimary: true, button: 0, buttons: type === "pointerup" ? 0 : 1, clientX: x, clientY: y,
    }));
    /** Presses a row (or a header), moves in steps to a point given by `to()` (read after the drag started), and holds there. */
    async function dragHold(el, to) {
      el.scrollIntoView({ block: "nearest" });
      const from = center(el);
      el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true, composed: true, pointerId: 11, pointerType: "mouse", isPrimary: true, button: 0, buttons: 1, clientX: from.x, clientY: from.y }));
      await sleep(30);
      ptr("pointermove", from.x, from.y - 8);
      await sleep(60);
      const end = to();
      for (let i = 1; i <= 6; i++) { ptr("pointermove", from.x + (end.x - from.x) * i / 6, from.y - 8 + (end.y - from.y + 8) * i / 6); await sleep(30); }
      return end;
    }
    const letGo = async (at) => { ptr("pointerup", at.x, at.y); await sleep(250); };
    async function listMenu() {
      if (!$(".ws-menu")) $(".ws-list-filter")?.click();
      return until("Group and sort menu", () => menuLabel(/新建组|New group/), 3000);
    }
    async function newGroupNamed(name) {
      (await listMenu())?.click();
      const input = await until("the new group's name field", () => $(".ws-sec-new input"), 3000);
      if (!input) throw sceneError("groups: no name field for a new group");
      setValue(input, name);
      keyDown(input, "Enter");
      return until(`group ${name}`, () => groupSec(name), 3000);
    }
    const ga = "E2E 发布前";
    const gb = "E2E 稍后";
    await clearBatch();
    const g1 = await newGroupNamed(ga);
    await check("New group… names a group in its header and switches the list to Custom groups", !!g1 && listPrefs().groupBy === "custom"
      && groupsSaved().groups.some((g) => g.name === ga), JSON.stringify({ prefs: listPrefs(), groups: groupsSaved().groups }));
    const g2 = await newGroupNamed(gb);
    await check("a second group follows the first, both drawn with the sessions in none under Ungrouped", !!g2
      && $$("[data-custom-group]").map((s) => s.dataset.customGroup).join() === [groupId(ga), groupId(gb)].join() && !!ungrouped());
    await check("an empty group says how to fill it", /拖到这里|Drag sessions here/.test(text($(".ws-group-empty", groupSec(ga)))));
    // SB-36 by keyboard: the row's menu, Move to group…, then the digit of the first group.
    await rowMenu(k1);
    menuLabel(/移动到组|Move to group/)?.click();
    const moveMenu = await until("Move to group menu", () => $(".ws-move-menu"), 3000);
    await check("Move to group lists the groups with digits, then New group…", !!moveMenu
      && text($(".ws-menu-key", menuLabel(new RegExp(`^${ga}$`)))) === "1" && !!menuLabel(/新建组|New group/), text(moveMenu));
    keyDown(document.activeElement ?? moveMenu, "1", { code: "Digit1" });
    await check("digit 1 moves the session into the first group, said in a toast", !!(await until("k1 in group A", () => inGroup(k1, ga), 3000))
      && $$(".ws-toast").some((el) => /移到|Moved/.test(text(el))));
    const moveToast = $$(".ws-toast").find((el) => /移到|Moved/.test(text(el)));
    $(".ws-toast-act", moveToast)?.click();
    await check("the toast's Undo takes it out again", !!(await until("k1 out of group A", () => !inGroup(k1, ga) && ungrouped()?.contains(rowOf(k1)), 3000)));
    await rowMenu(k1);
    menuLabel(/移动到组|Move to group/)?.click();
    (await until("Move to group menu again", () => menuLabel(new RegExp(`^${ga}$`)), 3000))?.click();
    await check("a click in Move to group moves it as well", !!(await until("k1 in group A again", () => inGroup(k1, ga), 3000)));
    // SB-42: drag a row into a group (the target's ring while held), Esc cancels a drag.
    let at = await dragHold($(".ws-row-main", rowOf(k2)), () => center($(".ws-gh", groupSec(gb))));
    await check("a row held over a group rings it and labels the drag", groupSec(gb)?.hasAttribute("data-drop-over") && !!$(".ws-drag-ghost")
      && rowOf(k2)?.hasAttribute("data-dragging"), text($(".ws-drag-ghost")));
    await letGo(at);
    await check("letting go moves the session into that group", !!(await until("k2 in group B", () => inGroup(k2, gb), 3000)) && !$(".ws-drag-ghost"));
    at = await dragHold($(".ws-row-main", rowOf(k3)), () => center($(".ws-gh", groupSec(ga))));
    keyDown(document.body, "Escape");
    await sleep(100);
    await letGo(at);
    await check("Esc cancels a drag: nothing moves, the drag's label goes, the open session stays", !inGroup(k3, ga) && !$(".ws-drag-ghost")
      && !$(".ws-row[data-dragging]") && currentKey() === kq);
    await shot("e01-custom-groups");
    // SB-42: Pinned's drop row pins (confirmed by the host), then a second row goes before it at the insertion line.
    at = await dragHold($(".ws-row-main", rowOf(k3)), () => center($("[data-drop=pin]")));
    await check("dragging a row that is not pinned turns Pinned's header into its drop row", !!$("[data-drop=pin][data-over]")
      && /松手即置顶|Let go to pin/.test(text($("[data-drop=pin]"))));
    // The click a real mouse sends right after letting go must not open the dragged session.
    ptr("pointerup", at.x, at.y);
    $(".ws-row-main", rowOf(k3))?.click();
    await sleep(250);
    await check("the click that ends a drag does not open the dragged session", currentKey() === kq, currentKey());
    const id3 = k3.slice(k3.indexOf("/") + 1);
    const id1 = k1.slice(k1.indexOf("/") + 1);
    await check("let go on the drop row: pinned here and on the host", !!(await until("k3 pinned", () => inPinned(k3), 5000))
      && !!(await batchHost("k3 pinned", (xs) => xs.some((x) => x.id === id3 && x.pinned === true))));
    at = await dragHold($(".ws-row-main", rowOf(k1)), () => { const r = rowOf(k3).getBoundingClientRect(); return { x: r.left + 40, y: r.top + 3 }; });
    const line = $(".ws-drop-line");
    await check("between pinned rows the drag shows an insertion line", !!line && Math.abs(line.getBoundingClientRect().top - rowOf(k3).getBoundingClientRect().top) <= 4,
      `${line?.getBoundingClientRect().top} vs ${rowOf(k3)?.getBoundingClientRect().top}`);
    await shot("e02-drag-insertion-line");
    await letGo(at);
    const pinnedOrder = () => $$("[data-pin-zone] .ws-row").map((r) => r.dataset.rowKey).filter((k) => k === k1 || k === k3);
    await check("let go at the line: pinned before the other, and still in its group", !!(await until("k1 pinned first", () => pinnedOrder().join() === [k1, k3].join(), 5000))
      && groupsSaved().members[k1] === groupId(ga) && !!(await batchHost("k1 pinned", (xs) => xs.some((x) => x.id === id1 && x.pinned === true))));
    // ⌥↓ on a pinned row moves it down within Pinned (SB-42 by keyboard).
    $(".ws-row-main", rowOf(k1)).focus();
    keyDown(document.activeElement, "ArrowDown", { altKey: true, code: "ArrowDown" });
    await check("⌥↓ moves a pinned row below the next one, and the order is kept here", !!(await until("k1 after k3", () => pinnedOrder().join() === [k3, k1].join(), 3000))
      && groupsSaved().pinOrder.indexOf(k3) < groupsSaved().pinOrder.indexOf(k1));
    await check("the moved row keeps the keyboard focus", !!(await until("focus kept", () => document.activeElement === $(".ws-row-main", rowOf(k1)), 2000)));
    // Out of Pinned: onto a group (moves in, unpins), and onto Ungrouped (unpins).
    at = await dragHold($(".ws-row-main", rowOf(k1)), () => center($(".ws-gh", groupSec(gb))));
    await letGo(at);
    await check("a pinned row dropped on a group moves there and is unpinned on the host", !!(await until("k1 in group B", () => inGroup(k1, gb) && !inPinned(k1), 5000))
      && !!(await batchHost("k1 unpinned", (xs) => xs.some((x) => x.id === id1 && x.pinned === false))));
    at = await dragHold($(".ws-row-main", rowOf(k3)), () => { const u = ungrouped(); u.scrollIntoView({ block: "nearest" }); return center($(".ws-gh", u)); });
    await letGo(at);
    await check("a pinned row dragged out onto Ungrouped is unpinned", !!(await until("k3 unpinned", () => !inPinned(k3) && ungrouped()?.contains(rowOf(k3)), 5000))
      && !!(await batchHost("k3 unpinned", (xs) => xs.some((x) => x.id === id3 && x.pinned === false))));
    // SB-15: a group's header dragged above the other, then ⌥↓ on it puts it back.
    const order = () => $$("[data-custom-group]").map((s) => s.dataset.customGroup).join();
    at = await dragHold($(".ws-gh-toggle", groupSec(gb)), () => { const r = groupSec(ga).getBoundingClientRect(); return { x: r.left + 60, y: r.top + 4 }; });
    await check("dragging a group's header shows the line between groups", !!$(".ws-drop-line") && groupSec(gb)?.hasAttribute("data-dragging"));
    await letGo(at);
    await check("let go: the group goes first, kept on this device", !!(await until("B first", () => order() === [groupId(gb), groupId(ga)].join(), 3000))
      && groupsSaved().groups.map((g) => g.name).join() === [gb, ga].join());
    $(".ws-gh-toggle", groupSec(gb)).focus();
    keyDown(document.activeElement, "ArrowDown", { altKey: true, code: "ArrowDown" });
    await check("⌥↓ on a group's header moves it back down", !!(await until("A first", () => order() === [groupId(ga), groupId(gb)].join(), 3000)));
    // SB-45: rename a group in place from its menu; delete it (second click), sessions back to Ungrouped, Undo.
    const groupMenu = async (name) => {
      const head = $(".ws-gh", groupSec(name));
      head.scrollIntoView({ block: "nearest" });
      const c = center(head);
      head.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, button: 2, clientX: c.x, clientY: c.y }));
      return until("group menu", () => menuLabel(/删除组|Delete group/), 3000);
    };
    await groupMenu(ga);
    menuLabel(/重命名组|Rename group/)?.click();
    const renameGroupField = await until("group rename field", () => $("input.ws-gh-rename", groupSec(ga)), 3000);
    const ga2 = "E2E 已改名";
    if (renameGroupField) { setValue(renameGroupField, ga2); keyDown(renameGroupField, "Enter"); }
    await check("a group is renamed in place (Enter keeps it)", !!(await until("renamed group", () => groupSec(ga2) && text($(".ws-gh-label", groupSec(ga2))) === ga2, 3000)));
    await groupMenu(gb);
    menuLabel(/删除组|Delete group/)?.click();
    const confirmDelete = await until("delete confirm", () => menuLabel(/再次点击|Click again/), 2000);
    await check("deleting a group asks for a second click", !!confirmDelete && !!groupSec(gb));
    confirmDelete?.click();
    await check("a deleted group's sessions go back to Ungrouped, said in a toast", !!(await until("B deleted", () => !groupSec(gb) && ungrouped()?.contains(rowOf(k1)) && ungrouped()?.contains(rowOf(k2)), 3000))
      && $$(".ws-toast").some((el) => /已删除组|Deleted/.test(text(el))));
    $(".ws-toast-act", $$(".ws-toast").find((el) => /已删除组|Deleted/.test(text(el))))?.click();
    await check("Undo brings the group back in its place with its sessions", !!(await until("B restored", () => inGroup(k1, gb) && inGroup(k2, gb), 3000))
      && order() === [groupId(ga2), groupId(gb)].join());
    // SB-04 / SB-28: comfortable rows and the metadata line, from Group and sort.
    await listMenu();
    menuLabel(/^舒适$|^Comfortable$/)?.click();
    menuLabel(/^元数据$|^Details$/)?.click();
    closeMenu();
    const rowHeight = () => Math.round(rowOf(kq)?.getBoundingClientRect().height ?? 0);
    await check("Comfortable makes rows 30 px; the metadata line adds 20 px with the folder", !!(await until("comfortable", () => $(".ws-side")?.dataset.density === "comfortable", 3000))
      && rowHeight() === 50 && rowOf(kq).hasAttribute("data-meta") && text($(".ws-row-meta", rowOf(kq))).includes(workName), `${rowHeight()} ${text($(".ws-row-meta", rowOf(kq)))}`);
    await shot("e03-comfortable-details");
    await listMenu();
    menuLabel(/^紧凑$|^Compact$/)?.click();
    menuLabel(/^元数据$|^Details$/)?.click();
    closeMenu();
    await check("Compact rows are 28 px again, one line", !!(await until("compact", () => rowHeight() === 28 && !rowOf(kq).hasAttribute("data-meta"), 3000)), rowHeight());
    // VS-34: a group opens with grid rows 0fr → 1fr and a fade (150 ms), none under "reduce motion".
    const head = $(".ws-gh-toggle", groupSec(gb));
    head.click();
    await until("B folded", () => groupSec(gb).hasAttribute("data-collapsed"), 2000);
    head.click();
    // React draws a click's update in a microtask: look a moment later, well inside the 150 ms.
    await sleep(20);
    const body = $(".ws-sec-body", groupSec(gb));
    const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
    const animation = body ? getComputedStyle(body).animationName : "";
    await check(`opening a group animates its rows (${reduce ? "reduce motion: none" : "150 ms grid rows and fade"})`, !!body?.hasAttribute("data-expanding")
      && (reduce ? animation === "none" : /ws-expand-rows/.test(animation) && /ws-expand-fade/.test(animation)), animation);
    await check("the opening ends by itself (no animation to wait for under reduce motion)", !!(await until("expanding ends", () => !body.hasAttribute("data-expanding"), 1500)));
    const motionRules = [...document.styleSheets].flatMap((s) => { try { return [...s.cssRules]; } catch { return []; } })
      .filter((r) => r instanceof CSSMediaRule && /prefers-reduced-motion:\s*no-preference/.test(r.conditionText ?? r.media.mediaText))
      .flatMap((r) => [...r.cssRules].map((x) => x.selectorText ?? ""));
    await check("the group animation is only declared for no motion preference", motionRules.some((sel) => sel.includes(".ws-sec-body[data-expanding]")), motionRules.join(" | ").slice(0, 200));
    // Back to the defaults (folders) for what follows; the groups stay on this device but are not drawn.
    await listMenu();
    menuLabel(/恢复默认|Restore defaults/)?.click();
    await check("Restore defaults goes back to folders, compact, no metadata line", !!(await until("defaults", () => listPrefs().groupBy === "project" && !$("[data-custom-group]") && $(".ws-side")?.dataset.density === "compact", 3000)));
    closeMenu();
    await check("groups never reached the host: its sessions carry only their own fields", !!(await batchHost("no group fields", (xs) => xs.every((x) => !("group" in x) && !("groups" in x)))));
    // The same live workspace stays mounted after the batch metadata checks. Exercise main's
    // conversation/project history with the batch restored before its expendable session is ended.
    await check("the live view reuses one complete workspace and one screen", $$(".nt-live-workspace .ws[data-embedded=true]").length === 1
      && $$(".nt-live-workspace .nt-stage").length === 1 && !$$(".nt-live-workspace .nt-chat-picker").length);
    const embedded = () => $(".nt-live-workspace .ws[data-embedded=true]");
    const liveCanvas = $(".nt-live-workspace .nt-stage canvas.rd-holder");
    await check("the real screen canvas is mounted beside the workspace", !!liveCanvas);
    // Stream zeroes its drawing size at start; only VideoDecoder's real output restores dimensions.
    // A DOM canvas alone (with the HTML default 300x150) is not proof that the host sent a frame.
    const liveFrameWaitStarted = performance.now();
    const decodedFrame = await until("a decoded frame from the isolated test host", () => liveCanvas?.isConnected
      && liveCanvas.width > 0 && liveCanvas.height > 0 && !$(".nt-live-workspace .session-error, .nt-live-workspace .stream-ended"), 30000);
    const sessionError = $(".nt-live-workspace .session-error");
    const streamEnded = $(".nt-live-workspace .stream-ended");
    // Error text stays local. Only these static classifications and typed state reach CI evidence;
    // never include the text, host/monitor labels, paths, frame contents or arbitrary exceptions.
    const localScreenError = `${text(sessionError)}\n${text(streamEnded)}`;
    const screenErrorCodes = [
      ["stream_terminal", /The screen stream ended\./i],
      ["wgc_unsupported", /Windows Graphics Capture is not supported here/i],
      ["wgc_start", /start Windows Graphics Capture/i],
      ["dda_start", /capture the screen with Desktop Duplication/i],
      ["capture_start", /start screen capture:/i],
      ["monitor_unavailable", /no such monitor:/i],
    ].filter(([, pattern]) => pattern.test(localScreenError)).map(([code]) => code);
    if ((sessionError || streamEnded) && !screenErrorCodes.length) screenErrorCodes.push("other");
    await say("e2e", `     (live screen diagnostic ${JSON.stringify({
      schema: 1, checkpoint: "workspace_live_frame", elapsed_ms: Math.max(0, Math.round(performance.now() - liveFrameWaitStarted)),
      document_hidden: document.hidden, secure_context: window.isSecureContext === true,
      video_decoder_available: typeof window.VideoDecoder === "function", encoded_video_chunk_available: typeof window.EncodedVideoChunk === "function",
      canvas_present: !!liveCanvas, canvas_connected: !!liveCanvas?.isConnected,
      width: liveCanvas ? liveCanvas.width : null, height: liveCanvas ? liveCanvas.height : null,
      session_error_present: !!sessionError, stream_ended_present: !!streamEnded,
      wait_satisfied: !!decodedFrame, error_codes: screenErrorCodes,
    })})`);
    await check("a real decoded screen frame has arrived without a capture error", !!decodedFrame,
      `${liveCanvas?.width}x${liveCanvas?.height}; ${text($(".nt-live-workspace .session-error, .nt-live-workspace .stream-ended"))}`);
    await note(`live-screen-continuity ${JSON.stringify({ phase: "begin", ts: Date.now(), host: CFG.host,
      decoded: !!decodedFrame, width: liveCanvas?.width, height: liveCanvas?.height })}`);
    const originalWorkspace = embedded();
    let canvasRemoved = false;
    const screenObserver = new MutationObserver(() => { if (!liveCanvas?.isConnected) canvasRemoved = true; });
    screenObserver.observe(document.body, { childList: true, subtree: true });
    const sameScreen = () => !canvasRemoved && !!liveCanvas?.isConnected
      && $(".nt-live-workspace .nt-stage canvas.rd-holder") === liveCanvas && embedded() === originalWorkspace;
    const paneSidebar = async () => {
      if (!$(".ws-side", embedded())) $(".ws-corner .ws-ib", embedded())?.click();
      return until("embedded session sidebar", () => $(".ws-side", embedded()), 5000);
    };
    await paneSidebar();
    await check("the existing session and project management is available beside the screen", !!rowOf(k1) && !!rowOf(kq)
      && !!$(".ws-side .ws-nav", embedded()) && !!$(".ws-sec", embedded()));
    $(".ws-row-main", rowOf(k1))?.click();
    await check("selecting another session changes the conversation in the same pane", !!(await until("other live-workspace session", () =>
      pageTitle() === name1 && $(".ws-session .nt-chat", embedded()), 30000)));
    await check("switching sessions on one computer keeps its screen canvas and workspace mounted", sameScreen());
    await paneSidebar();
    $(".ws-side .ws-nav", embedded())?.click();
    await check("New opens the existing CLI, project and first-message composer beside the screen", !!(await until("embedded new session", () =>
      $(".ws-new .ws-composer textarea", embedded()) && $(".ws-env-trail .ws-chip", embedded()), 10000)));
    await check("choosing a project on the same computer keeps its screen canvas mounted", sameScreen());
    await check("the embedded new-session page offers the existing CLI selector", await pickAgent("Rich fake"));
    await check("the embedded project picker uses the existing recent project", !!(await pickRecent(workName)), folderShown());
    // The isolated host permits four agents. Keep the two sessions used by the history checks, and
    // retire this run's unused renamed session before testing another real first-message creation.
    if (!k2?.startsWith(`${CFG.host}/`)) throw new Error("the expendable test session does not belong to this isolated host");
    const unusedSession = k2.slice(CFG.host.length + 1);
    await hostCall("chat_end", { session: unusedSession });
    let unusedEnded = false;
    const endDeadline = Date.now() + 30000;
    while (Date.now() < endDeadline) {
      const report = await hostCall("chat_sessions");
      if (report.sessions.some((session) => session.id === unusedSession && session.state === "ended")) { unusedEnded = true; break; }
      await sleep(250);
    }
    await check("the run's unused session has ended before another first message starts", unusedEnded);
    if (!unusedEnded) throw new Error("the unused test session never ended, so creating another would exceed the host's agent limit");
    // D did not add a session. B ended k3 before kq started; only k2 is retired here, after its
    // batch/Undo metadata was verified. Keep k1, kb and kq, including the IDs needed by C's Back flow.
    const capacity = await hostCall("chat_sessions");
    const retainedIds = [k1, kb, kq].map((key) => {
      if (!key.startsWith(`${CFG.host}/`)) throw new Error("a retained test session belongs to another host");
      return key.slice(CFG.host.length + 1);
    });
    const activeIds = capacity.sessions.filter((session) => session.state !== "ended").map((session) => session.id);
    const capacityReady = sameKeys(activeIds, retainedIds) && activeIds.length <= 3
      && capacity.sessions.some((session) => k3.endsWith(`/${session.id}`) && session.state === "ended")
      && (!Number.isFinite(capacity.live) || capacity.live <= 3);
    await check("the four-agent host retains only the three required sessions before C creates its fourth", capacityReady,
      JSON.stringify({ activeIds, retainedIds, live: capacity.live, endedUnused: unusedSession }));
    if (!capacityReady) throw new Error("the isolated host has no verified free slot for C's new session");
    const firstInPane = $(".ws-new .ws-composer textarea", embedded());
    setValue(firstInPane, "实时画面旁直接开始一个新会话");
    const sendFirst = await until("embedded first message ready", () => $(".ws-new .ws-send:not(:disabled)", embedded()), 5000);
    sendFirst?.click();
    await check("the first message starts a real session without leaving the screen", !!(await until("embedded first-message reply", () => {
      const chat = $(".ws-session .nt-chat", embedded());
      return chat?.textContent.includes("you said: 实时画面旁直接开始一个新会话") && chat;
    }, 30000)));
    await check("starting a session keeps the current computer's screen and workspace mounted", sameScreen());
    // The workspace's own history buttons remain available even when its sidebar is folded.
    $$(".ws-corner .ws-ib", embedded())[1]?.click();
    await check("Back returns to the embedded new-session page", !!(await until("embedded history new page", () =>
      $(".ws-new .ws-composer textarea", embedded()), 10000)));
    $$(".ws-corner .ws-ib", embedded())[1]?.click();
    await check("Back selects the previous conversation inside the live workspace", !!(await until("embedded history session", () =>
      pageTitle() === name1 && $(".ws-session .nt-chat", embedded()), 30000)));
    await check("conversation history keeps the live screen mounted", sameScreen());
    screenObserver.disconnect();
    await shot("c03-live-workspace-history");
    // Back to the conversation this sequence started from; there is no separate Watch page.
    $$(".ws-corner .ws-ib", embedded())[1]?.click();
    await check("Back returns to the original conversation beside the same screen", !!(await until("initial live-workspace conversation", () =>
      $(".ws-session .nt-chat", embedded())?.textContent.includes("SLOW 排队测试"), 30000)) && sameScreen());
    await check("Back returns to the same workspace session", currentKey() === kq && !!readyConversation(kq),
      `${currentKey()} vs ${kq}`);
    await note(`live-screen-continuity ${JSON.stringify({ phase: "end", ts: Date.now(), host: CFG.host,
      sameCanvas: sameScreen(), width: liveCanvas?.width, height: liveCanvas?.height })}`);
    keyDown($(".nt-live-divider"), "Home");
    const paneFolded = await until("sidebar folded inside its narrower pane", () => embedded()?.dataset.side === "hidden", 5000);
    await check("the sidebar folds by workspace pane width even in a wide app window", !!paneFolded, `${innerWidth} / ${embedded()?.clientWidth}`);
    await paneSidebar();
    await check("the existing toggle opens session and project management over the narrow pane", embedded()?.dataset.side === "float" && !!rowOf(kq));
    await shot("c04-live-workspace-narrow-sidebar");
  } catch (e) {
    await check("no script errors", false, e && e.stack || e);
  }
  // Leave the shared storage as it was: out of the workspace first (it stops writing its cache), and a
  // moment for the web view to store it before the app quits.
  leaving = true;
  $(".ws-modes [data-mode=computers]")?.click();
  await sleep(500);
  for (const k of Object.keys(localStorage)) if (KEEP.test(k) && !(k in saved)) localStorage.removeItem(k);
  for (const [k, v] of Object.entries(saved)) localStorage.setItem(k, v);
  await sleep(1500);
  await say("e2e", `${checks} checks`);
  await say("e2e", failures ? `${failures} failed` : "all passed");
  await say("done", String(failures));
})();
