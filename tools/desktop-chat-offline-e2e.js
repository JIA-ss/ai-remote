// A message sent while the host is gone shows as not sent and goes through on Retry once the host is
// back (run with E2E_SCRIPT=tools/desktop-chat-offline-e2e.js python3 tools/desktop-chat-e2e.py; the
// harness stops and starts the host on "(action) stop-host" / "(action) start-host").
(async () => {
  const CFG = __CONFIG__;
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
      try { const v = fn(); if (v) return v; } catch {}
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
  const composer = () => $(".nt-compose textarea");
  const keyDown = (el, key) => el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
  async function send(text) {
    const ta = await until("composer", composer);
    setValue(ta, text);
    await sleep(100);
    $(".nt-compose .round.send").click();
  }

  // The complete workspace owns creation and navigation beside the live screen.
  const readyConversation = () => {
    const pane = $(".ws-session .nt-chat");
    return pane && !$(".cl-loading, .cdx-loading, .nt-chat-status", pane)
      && $(".nt-compose textarea:not(:disabled)", pane) && pane;
  };
  async function pickAgent(name) {
    const chip = await until("workspace agent chip", () => $(".ws-env-trail .ws-chip"), 30000);
    if (!chip) throw new Error("the workspace did not offer an agent");
    if ($(".ws-chip-text", chip)?.textContent.trim() === name) return true;
    chip.click();
    const choice = await until(`workspace agent ${name}`, () => $$(".ws-menu .ws-menu-item").find((b) => $(".ws-menu-label", b)?.textContent.trim() === name), 5000);
    if (!choice) throw new Error(`the workspace did not list ${name}`);
    choice.click();
    return until("workspace agent chosen", () => $(".ws-env-trail .ws-chip-text")?.textContent.trim() === name, 5000);
  }
  async function startSession(prompt) {
    const ta = await until("workspace first-message composer", () => $(".ws-new .ws-composer textarea"), 30000);
    if (!ta) throw new Error("the workspace new-session page is absent");
    setValue(ta, prompt);
    const send = await until("first message ready", () => $(".ws-new .ws-send:not(:disabled)"));
    if (!send) throw new Error("the workspace never enabled its first-message Send");
    send.click();
    return until("connected workspace conversation", readyConversation, 30000);
  }
  try {
    // Start the stand-in in the work folder, typed into the picker.
    await until("start form", () => $(".ws-new .ws-composer textarea"), 30000);
    await pickAgent("Rich fake");
    $(".ws-chip-folder .nt-folder-btn").click();
    const q = await until("folder field", () => $("#ag-folder-q"));
    setValue(q, CFG.work);
    await until("open folder path", () => $(".nt-folder-list .row.open"));
    keyDown(q, "Enter");
    const work = CFG.work.split(/[\\/]/).pop();
    (await until("use button", () => $(".nt-folder-crumbs .cur")?.textContent === work && $(".nt-folder-ft .btn.primary:not(:disabled)"))).click();
    await until("picker closed", () => !$(".nt-folder-pop"), 5000);
    await startSession("first");
    await check("the conversation starts", !!(await until("first answer", () => document.body.textContent.includes("you said: first"), 30000)));

    // The host goes away: the panel loses the chat; a message sent now is not sent.
    await say("e2e", "(action) stop-host");
    await until("disconnected", () => /连接|connect/i.test($(".nt-chat-status")?.textContent ?? ""), 20000);
    const t0 = Date.now();
    await send("while offline");
    const failed = await until("not sent", () => $(".nt-sending.failed"), 25000);
    await check("a message sent while the host is gone shows as not sent", !!failed, $(".nt-sending")?.textContent);
    const bubble = $(".bubble", failed);
    await check("the undelivered message text stays visible", !!bubble && getComputedStyle(bubble).opacity !== "0" && bubble.getBoundingClientRect().height > 0);
    await check("it says why and offers Retry and Discard", !!failed && /连接断开|connection dropped|没有响应|did not answer/.test(failed.textContent)
      && $$(".nt-ib", failed).length === 2, failed?.textContent);
    await say("e2e", `     (not sent after ${((Date.now() - t0) / 1000).toFixed(1)} s)`);
    await shot("20-offline-not-sent");

    // The host comes back; the panel reconnects; Retry sends it, once.
    await say("e2e", "(action) start-host");
    await check("the panel reconnects", !!(await until("reconnected", readyConversation, 60000)),
      $(".nt-chat-status")?.textContent);
    $$(".nt-ib", $(".nt-sending.failed"))[0].click();
    await check("Retry delivers it", !!(await until("delivered", () => !$(".nt-sending") && document.body.textContent.includes("you said: while offline"), 30000)),
      $(".nt-sending")?.textContent);
    await sleep(2000);
    const runs = (document.body.textContent.match(/you said: while offline/g) ?? []).length;
    await check("and it runs once", runs === 1, `${runs} answers`);
    await shot("21-offline-retried");
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
