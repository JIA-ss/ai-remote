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
  async function openRow(key) {
    $(".ws-row-main", rowOf(key))?.click();
    return until(`session ${key} open`, () => currentKey() === key && $(".ws-session"));
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
  async function send(t) {
    // Opening a row mounts its composer before the new chat attachment has finished connecting.
    // Sending in that gap deliberately creates an undelivered message instead of reaching the agent.
    const ta = await until("connected composer", () => !$(".ws-main .nt-chat-status, .ws-main .cl-loading") && composer());
    if (!ta) throw new Error(`the conversation did not connect: ${text($(".ws-main .nt-chat-status"))}`);
    setValue(ta, t);
    const button = await until("message ready to send", () => $(".ws-main .nt-compose .round.send:not(:disabled)"));
    if (!button) throw new Error("the composer never enabled Send");
    button.click();
    await until("composer cleared", () => composer().value === "", 5000);
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
  const KEEP = /^ai-remote\.(mode$|ws\.)/;
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
    const ws = await until("workspace", () => $(".ws .ws-new"), 10000);
    await check("the switch opens the agents workspace on a new session", !!ws && localStorage.getItem("ai-remote.mode") === "agents", localStorage.getItem("ai-remote.mode"));
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
    // An occluded/locked desktop polls at the store's idle 20 s cadence, instead of 4 s. Observe
    // the natural poll; do not force a refresh or fail before that configured interval can elapse.
    const settledAt = Date.now();
    await check("the ended session has no dot (it is open)", !!(await until("no dot", () => dotOf(k1) === null, 35000)),
      `${dotOf(k1)}; ${(Date.now() - settledAt) / 1000} s; visibility: ${document.visibilityState}`);

    // A2. Unread: a session that moves on while another is open gets a blue dot; opening it clears it.
    await newSession();
    await check("New keeps the last place", folderShown() === workName, folderShown());
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
    await send("FAIL 这一轮会失败");
    await failedSnapshot("sent");
    await check("a failed turn shows a red dot", !!(await until("failed dot", () => dotOf(k2) === "failed", 20000)), dotOf(k2));
    await failedSnapshot("after-dot-wait");
    await shot("a04-failed");
    await rowMenu(k2);
    const done = byLetter("U");
    await check("its menu offers Mark as done", !!done && /标记为完成|Mark as done/.test(text(done)), text(done));
    done?.click();
    await check("Mark as done clears the red dot", !!(await until("done", () => dotOf(k2) === null, 5000)), dotOf(k2));
    await check("and says so in a toast", !!(await until("toast", () => $(".ws-toast"), 3000)));

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
    await check("renaming does not bring back the red dot put aside with Mark as done", dotOf(k2) !== "failed", dotOf(k2));
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
    $(".ws-list-filter").click();
    (await until("list menu", () => filter().length >= 12 && filter(), 5000))?.[10].click();
    const back = await until("archived row", () => rowOf(k2)?.hasAttribute("data-archived") && rowOf(k2), 10000);
    await check("the archived view lists it, marked", !!back);
    await sleep(5000); // a poll with the archived list
    await check("the open session's page stays while the list shows archived sessions", !!$(".ws-session .nt-chat") && !$(".ws-session .ws-state"),
      text($(".ws-session .ws-state")) || "");
    await shot("a06-archived");
    filter()[11]?.click();
    await sleep(5000);
    await check("the All view lists archived and other sessions together", !!rowOf(k2) && !!rowOf(k1), `${!!rowOf(k1)} ${!!rowOf(k2)}`);
    filter()[10]?.click();
    await until("archived row", () => rowOf(k2), 10000);
    closeMenu();
    await until("menu closed", () => !$(".ws-menu"), 3000);
    await rowMenu(k2);
    const unarch = await until("Unarchive item", () => byLetter("A"), 3000);
    await check("an archived row's menu offers Unarchive", /取消归档|Unarchive/.test(text(unarch)), $(".ws-menu")?.textContent);
    unarch?.click();
    await check("Unarchive takes it out of the archived view, with a toast", !!(await until("unarchived", () => !rowOf(k2) && $$(".ws-toast").some((x) => /取消归档|Unarchived/.test(x.textContent)), 10000)));
    $(".ws-list-filter").click();
    (await until("list menu", () => filter().length >= 12 && filter(), 5000))?.[9].click();
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
    await shot("a07-codex");

    // A8. ⌘K: by title, by folder; Enter opens.
    chord("k");
    const pq = await until("palette", () => $(".ws-palette input"), 5000);
    await check("⌘K opens the search", !!pq);
    setValue(pq, "重命名");
    const hit = await until("title hit", () => $$(".ws-palette-row").length === 1 && text($(".ws-palette-title")) === name2, 5000);
    await check("it finds a session by its title", !!hit, $$(".ws-palette-row").map(text).join(" | "));
    await shot("a08-search");
    keyDown(pq, "Enter");
    await check("Enter opens it", !!(await until("opened", () => !$(".ws-palette") && pageTitle() === name2, 5000)), pageTitle());
    chord("k");
    const pq2 = await until("palette", () => $(".ws-palette input"), 5000);
    setValue(pq2, "src");
    const byFolder = await until("folder hit", () => $$(".ws-palette-row").length >= 1 && $$(".ws-palette-row").every((r) => text($(".ws-palette-where", r)).includes("src")) && $$(".ws-palette-row"), 5000);
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

    // The live view uses this same conversation. Opening it must carry the session ID, rather than
    // silently taking an unrelated session remembered by the former side panel.
    $(".ws-tb-end .ws-ib").click();
    const rail = await until("this conversation beside the live view", () => {
      const el = $(".win[data-route=session] .nt-rail .nt-chat");
      return el && el.textContent.includes("SLOW 排队测试") && el.textContent.includes("you said: 排队一条（已改）") && el;
    }, 30000);
    await check("Watch opens the current conversation beside the live view", !!rail && rail.classList.contains("cl-root"));
    await check("the live view has no old steps or agent-perspective tabs", !$(".nt-rail > .seg"));
    await shot("c03-watch-current-session");
    $(".gl-toolbar .back:not(:disabled)").click();
    await check("Back returns to the same workspace session", !!(await until("workspace session again", () => $(".ws-session") && currentKey() === kq)));
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
