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
  const composer = () => $(".nt-compose textarea");
  const keyDown = (el, key) => el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
  async function send(text) {
    const ta = await until("composer", composer);
    setValue(ta, text);
    await sleep(100);
    $(".nt-compose .round.send").click();
  }
  try {
    // Start the stand-in in the work folder, typed into the picker.
    await until("start form", () => $(".nt-chat-start"), 30000);
    $$(".nt-chat-pick .opt").find((b) => b.textContent.includes("Rich fake")).click();
    $(".nt-chat-start .nt-folder-btn").click();
    const q = await until("folder field", () => $("#ag-folder-q"));
    setValue(q, CFG.work);
    keyDown(q, "Enter");
    const work = CFG.work.split(/[\\/]/).pop();
    (await until("use button", () => $(".nt-folder-crumbs .cur")?.textContent === work && $(".nt-folder-ft .btn.primary:not(:disabled)"))).click();
    await until("picker closed", () => !$(".nt-folder-pop"), 5000);
    setValue($("#ag-prompt"), "first");
    await sleep(200);
    $$(".nt-chat-start .btn.primary").at(-1).click();
    await check("the conversation starts", !!(await until("first answer", () => document.body.textContent.includes("you said: first"), 30000)));

    // The host goes away: the panel loses the chat; a message sent now is not sent.
    await say("e2e", "(action) stop-host");
    await until("disconnected", () => /连接|connect/i.test($(".nt-chat-status")?.textContent ?? ""), 20000);
    const t0 = Date.now();
    await send("while offline");
    const failed = await until("not sent", () => $(".nt-sending.failed"), 25000);
    await check("a message sent while the host is gone shows as not sent", !!failed, $(".nt-sending")?.textContent);
    await check("it says why and offers Retry and Discard", !!failed && /连接断开|connection dropped|没有响应|did not answer/.test(failed.textContent)
      && $$(".nt-ib", failed).length === 2, failed?.textContent);
    await say("e2e", `     (not sent after ${((Date.now() - t0) / 1000).toFixed(1)} s)`);
    await shot("20-offline-not-sent");

    // The host comes back; the panel reconnects; Retry sends it, once.
    await say("e2e", "(action) start-host");
    await check("the panel reconnects", !!(await until("reconnected", () => !$(".nt-chat-status") || !/连接|connect/i.test($(".nt-chat-status").textContent), 60000)),
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
