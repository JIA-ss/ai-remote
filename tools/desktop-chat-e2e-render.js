// Rendering checks for the chat panel that tools/desktop-chat-e2e.js does not cover, for a webview other
// than the one it was built on (WebView2 on Windows): a long message folds under a mask, far turns skip
// rendering (content-visibility), Enter while an input method composes does not send, the list follows the
// newest turn, and which fonts the page actually uses. Run it with the same harness:
//   E2E_SCRIPT=tools/desktop-chat-e2e-render.js python tools/desktop-chat-e2e.py
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
      try {
        const v = fn();
        if (v) return v;
      } catch {}
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
  const running = () => !!$(".nt-compose .round.stop");
  const turns = () => $$(".nt-turn");
  const idle = (ms = 20000) => until("turn end", () => !running() && !$(".nt-working") && turns().at(-1)?.querySelector(".nt-foot, .nt-stopped"), ms);
  async function send(text) {
    const ta = await until("composer", composer);
    setValue(ta, text);
    await sleep(100);
    $(".nt-compose .round.send").click();
    await until("composer cleared", () => composer().value === "", 5000);
    await idle();
  }
  // The folder picker (ui/chat/FolderPicker.tsx): a typed path, Enter browses it, the footer takes it.
  async function typeFolder(path) {
    if (!$(".nt-folder-pop")) $(".nt-chat-start .nt-folder-btn").click();
    const q = await until("folder search box", () => $("#ag-folder-q"));
    setValue(q, path);
    await until("open-path row", () => $(".nt-folder-list .row.open"));
    q.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    const name = path.split(/[\\/]/).filter(Boolean).pop();
    (await until("folder listed", () => $(".nt-folder-crumbs .cur")?.textContent === name && $(".nt-folder-ft .btn.primary:not(:disabled)")))?.click();
    await until("picker closed", () => !$(".nt-folder-pop"), 5000);
  }
  try {
    await until("start form", () => $(".nt-chat-start"), 30000);
    $$(".nt-chat-pick .opt").find((b) => b.textContent.includes("Rich fake")).click();
    await typeFolder(CFG.work);
    setValue($("#ag-prompt"), "RICH fix the quarter export");
    await sleep(200);
    $(".nt-chat-start .btn.primary").click();
    await until("conversation", () => $(".nt-chat .nt-chat-hd"), 30000);
    await idle(30000);

    // Fonts the page resolves to (the bundled faces, or the platform's).
    await document.fonts.ready;
    const fam = (sel) => { const el = $(sel); return el ? getComputedStyle(el).fontFamily : "(none)"; };
    const loaded = [...document.fonts].filter((f) => f.status === "loaded").map((f) => `${f.family} ${f.weight}`);
    await check("fonts", loaded.length > 0, `body: ${fam("body")} | reply: ${fam(".nt-md")} | code: ${fam(".nt-md pre, .nt-cmd")} | loaded: ${[...new Set(loaded)].join(", ")}`);
    await check("CJK text has a face", document.fonts.check("14px " + getComputedStyle($(".nt-md")).fontFamily, "读了文件"));

    // A long message folds behind a fading mask.
    const long = Array.from({ length: 40 }, (_, i) => `line ${i + 1}: 这是一条很长的消息，用来检查折叠 the long message folds`).join("\n");
    await send(long);
    const bubble = $$(".nt-user .bubble").at(-1);
    const txt = bubble && $(".txt", bubble);
    const mask = txt ? (getComputedStyle(txt).maskImage || getComputedStyle(txt).webkitMaskImage) : "";
    await check("a long message folds", !!bubble && bubble.classList.contains("folded"), bubble?.className);
    await check("the fold fades out with mask-image", /gradient/.test(mask ?? ""), mask);
    await check("the folded text is cut short", !!txt && txt.clientHeight < txt.scrollHeight, `${txt?.clientHeight} < ${txt?.scrollHeight}`);
    bubble?.scrollIntoView({ block: "center" });
    await sleep(300);
    await shot("10-long-folded");

    // Enter during composition (an input method) must not send; plain Enter does.
    const ta = composer();
    setValue(ta, "ime test");
    ta.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
    ta.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", code: "Enter", keyCode: 229, isComposing: true, bubbles: true, cancelable: true }));
    await sleep(400);
    await check("Enter while composing does not send", composer().value === "ime test", composer().value);
    ta.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true, data: "" }));
    ta.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", code: "Enter", keyCode: 13, bubbles: true, cancelable: true }));
    await until("sent", () => composer().value === "", 5000);
    await check("plain Enter sends", composer().value === "", composer().value);
    await idle();

    // Enough turns for the far ones to skip rendering; the list stays at the newest.
    for (let i = 0; i < 3; i++) await send(`filler ${i}`);
    const cv = turns().map((t) => getComputedStyle(t).contentVisibility);
    await check("far turns use content-visibility: auto, near ones do not", cv[0] === "auto" && cv.at(-1) !== "auto", cv.join(","));
    const body = $(".nt-chat-body");
    await check("the list follows the newest turn", body.scrollHeight - body.scrollTop - body.clientHeight < 40,
      `${body.scrollHeight} - ${body.scrollTop} - ${body.clientHeight}`);
    body.scrollTop = 0;
    await sleep(300);
    await check("far turns still paint when scrolled to", turns()[0].getBoundingClientRect().height > 50, turns()[0].getBoundingClientRect().height);
    await shot("11-scrolled-top");
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
