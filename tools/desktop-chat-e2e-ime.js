// Typing Chinese through a real input method into the chat composer (Windows, Microsoft Pinyin): the keys are
// real key events (tools/win_input.py, asked for with "(action) input {...}" lines), so the input method composes
// as it does for a person. Checks that Enter while composing only confirms the composition and never sends, that
// a candidate picked with Space lands in the composer, and that Enter then sends it.
//   E2E_SCRIPT=tools/desktop-chat-e2e-ime.js python tools/desktop-chat-e2e.py --native-input
// It moves the mouse and types on that computer's real desktop, and switches the app window's keyboard layout to
// Chinese (0804) and back to US English (0409) at the end.
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
  // The harness carries the action out before it reads the next line, so a short pause after is enough.
  const act = async (a, ms = 800) => { await say("e2e", `(action) input ${JSON.stringify(a)}`); await sleep(ms); };
  const composer = () => $(".nt-compose textarea");
  const running = () => !!$(".nt-compose .round.stop");
  const turns = () => $$(".nt-turn");
  const users = () => $$(".nt-user").map((u) => $(".txt", u)?.textContent ?? u.textContent);
  const idle = (ms = 20000) => until("turn end", () => !running() && !$(".nt-working") && turns().at(-1)?.querySelector(".nt-foot, .nt-stopped"), ms);

  // What the input method does to the composer.
  const seen = { start: 0, end: 0, enterComposing: 0, data: [] };
  document.addEventListener("compositionstart", () => seen.start++, true);
  document.addEventListener("compositionend", (e) => { seen.end++; seen.data.push(e.data); }, true);
  document.addEventListener("keydown", (e) => { if (e.key === "Enter" && (e.isComposing || e.keyCode === 229)) seen.enterComposing++; }, true);

  try {
    await until("start form", () => $(".nt-chat-start"), 30000);
    $$(".nt-chat-pick .opt").find((b) => b.textContent.includes("Rich fake")).click();
    // The folder: typed into the picker (ui/chat/FolderPicker.tsx), its work folder entered, then taken.
    const work = CFG.work.split(/[\\/]/).pop();
    $(".nt-chat-start .nt-folder-btn").click();
    await until("home listing", () => $$(".nt-folder-list .row.child").length > 0 && $(".nt-folder-crumbs"));
    const q = await until("folder field", () => $("#ag-folder-q"));
    setValue(q, CFG.work.slice(0, CFG.work.length - work.length - 1));
    await until("open row", () => $(".nt-folder-list .row.open"));
    q.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    (await until("work folder", () => $$(".nt-folder-list .row.child").find((r) => r.querySelector(".fn")?.textContent === work))).click();
    await until("work folder entered", () => $(".nt-folder-crumbs .cur")?.textContent === work);
    (await until("use button", () => $(".nt-folder-ft .btn.primary:not(:disabled)"))).click();
    await check("the work folder is chosen", !!(await until("folder chosen", () => $(".nt-chat-start .nt-folder-btn .nt-folder-lbl .fn")?.textContent === work)));
    setValue($("#ag-prompt"), "hello");
    await sleep(200);
    $(".nt-chat-start .btn.primary").click();
    await until("conversation", () => $(".nt-chat .nt-chat-hd"), 30000);
    await idle(30000);

    const ta = composer();
    setValue(ta, "");
    await act({ focus: true }, 1000);
    const r = ta.getBoundingClientRect();
    await act({ click: [Math.round((r.left + r.width / 2) * devicePixelRatio), Math.round((r.top + r.height / 2) * devicePixelRatio)] });
    await check("the composer has the keyboard focus", document.hasFocus() && document.activeElement === ta,
      `hasFocus ${document.hasFocus()}, active ${document.activeElement?.tagName}`);
    await act({ layout: "00000804" }, 800);

    // 1. Letters compose; Enter confirms the letters and sends nothing.
    const before = users().length;
    // The input method may start in its English mode, or switched off: try what a person would press.
    let how = "";
    for (const [name, toggle] of [["as it is", null], ["Shift", { key: "SHIFT" }], ["Ctrl+Space", { chord: ["CONTROL", "SPACE"] }],
                                  ["Ctrl+Space, Shift", { key: "SHIFT" }]]) {
      setValue(composer(), "");
      if (toggle) await act(toggle, 500);
      await act({ type: "nihao" }, 300);
      // The first composition of a new input context can take a few seconds to start.
      if (await until(`composition (${name})`, () => seen.start > 0, 5000)) { how = name; break; }
      await act({ key: "ESCAPE" }, 300);
    }
    await say("e2e", `     (composition started with: ${how || "nothing"})`);
    await check("typing letters starts a composition", seen.start > 0, JSON.stringify(seen));
    await shot("20-ime-composing");
    await act({ key: "RETURN" }, 1200);
    // WebView2 keeps the Enter the input method consumed from the page (no keydown at all); Chrome sends it as
    // keyCode 229. The composer handles both; this only records which.
    await say("e2e", `     (Enter while composing: ${seen.enterComposing} keydown(s) reached the page; ${JSON.stringify(seen)})`);
    await check("Enter while composing sends nothing", users().length === before, users().slice(before).join(" | "));
    await check("Enter confirms the letters into the composer", /nihao/i.test(composer().value), JSON.stringify(composer().value));

    // 2. Space picks the first candidate; Enter then sends it.
    setValue(composer(), "");
    await act({ type: "nihao" }, 1200);
    await act({ key: "SPACE" }, 800);
    const picked = composer().value;
    await check("Space puts the Chinese candidate in the composer", /[一-鿿]/.test(picked), JSON.stringify(picked));
    await shot("21-ime-picked");
    await act({ key: "RETURN" }, 600);
    const sent = await until("message sent", () => users().length > before && users().at(-1).includes(picked.trim()), 10000);
    await check("Enter after the composition sends the Chinese text", !!sent && picked.trim().length > 0, users().slice(before).join(" | "));
    await check("the composer is empty after sending", composer().value === "", JSON.stringify(composer().value));
    await idle();
    await check("the agent got the Chinese text", $$(".nt-reply").some((x) => x.textContent.includes(picked.trim())), $$(".nt-reply").at(-1)?.textContent);
    await shot("22-ime-sent");
  } catch (e) {
    await check("no script errors", false, e && e.stack || e);
  }
  await act({ layout: "00000409" }, 300);
  await act({ restore: true }, 300);
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
