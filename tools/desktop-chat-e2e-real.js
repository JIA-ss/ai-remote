// The desktop chat panel with a real agent (tools/desktop-chat-e2e.py --real <name>): a small file task,
// a project command picked from the / menu, a pasted image, then a steer while it works. Same hooks as desktop-chat-e2e.js.
(async () => {
  const CFG = __CONFIG__;
  // The test window is off screen (the page is hidden, requestAnimationFrame never fires); the app
  // folds events on a timer then, which these runs rely on.
  const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);
  const say = (level, message) => invoke("log_from_page", { level, message });
  let failures = 0;
  const T0 = Date.now();
  const check = (name, ok, detail = "") => {
    if (!ok) failures++;
    return say("e2e", `${ok ? "ok  " : "FAIL"}  [${Math.round((Date.now() - T0) / 1000)} s] ${name}${detail ? `  (${String(detail).slice(0, 300)})` : ""}`);
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
    // The page must stay on this state until the native capture has finished.
    try {
      Promise.resolve(say("shot", name)).catch((error) => finish(String(error) || "screenshot request rejected"));
    } catch (error) {
      finish(String(error) || "screenshot request rejected");
    }
  });
  function setValue(el, v) {
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, "value").set.call(el, v);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  }
  const running = () => !!$(".nt-compose .round.stop");
  const lastTurn = () => $$(".cl-turn").at(-1);
  // A completed group reports its command count in its sentence. While running, open its rows and
  // count the actual Bash bodies: descriptions can say "Echoing N" rather than naming the shell.
  const openGroups = (turn) => {
    for (const b of $$(".cl-group > .cl-group-hd[aria-expanded=false]", turn)) b.click();
  };
  const openTools = (turn) => {
    for (const b of $$(".cl-tool:not(.cl-thought) > .cl-tool-hd[aria-expanded=false]:not(:disabled)", turn)) b.click();
  };
  const commands = (turn) => {
    if (!turn) return 0;
    openGroups(turn);
    openTools(turn);
    let n = 0;
    for (const group of $$(".cl-group", turn)) {
      const summary = $(".cl-group-hd .cl-sent", group);
      if (summary) {
        for (const m of summary.textContent.matchAll(/(?:运行了?|[Rr]an|[Rr]unning) ?(\d+) ?(?:条命令|commands?)/g)) n += Number(m[1]);
      } else n += $$(".cl-bash", group).length;
    }
    return n + $$(".cl-bash", turn).filter((x) => !x.closest(".cl-group")).length;
  };
  // The folder picker (ui/chat/FolderPicker.tsx): open it, type a path, Enter browses it, the footer takes it.
  const keyDown = (el, key) => el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
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
  async function send(text) {
    const ta = await until("composer", () => $(".nt-compose textarea"));
    setValue(ta, text);
    await sleep(100);
    $(".nt-compose .round.send").click();
  }
  const idle = (ms) => until("turn end", () => !running() && !$(".cl-status") && lastTurn()?.querySelector(".cl-foot, .cl-stopped"), ms);

  try {
    await until("start form", () => $(".nt-chat-start"), 30000);
    const pick = $$(".nt-chat-pick .opt").find((b) => b.querySelector(".nm")?.textContent === CFG.agent);
    await check(`${CFG.agent} is offered`, !!pick, $$(".nt-chat-pick .opt").map((b) => b.textContent).join(" | "));
    pick.click();
    await typeFolder(CFG.work);
    await check("the folder picker takes a typed path", folderShown() === CFG.work.split(/[\\/]/).pop(), folderShown());
    setValue($("#ag-prompt"), "Create a file named hello.txt containing the single line: hi from ai-remote. Then run `cat hello.txt` in the shell. Answer with one short sentence.");
    await sleep(200);
    $(".nt-chat-start .btn.primary").click();
    await check("the real agent starts", !!(await until("conversation", () => $(".nt-chat .nt-chat-hd"), 60000)));
    await check("its first turn finishes", !!(await idle(180000)));
    const t1 = lastTurn();
    openGroups(t1);
    await until("tool rows", () => $(".cl-tool", t1), 5000);
    openTools(t1);
    await until("shell body", () => $(".cl-bash", t1), 5000);
    await check("the turn completed (not failed)", !$(".nt-notice.error, .cl-fail", t1) && !$(".cl-stopped", t1), t1?.textContent.slice(-300));
    const edit = $(".cl-diff", t1) || $(".cl-group", t1) || $(".cl-tool", t1);
    await check("the agent's tool calls show (edit card, run or tool line)", !!edit, t1?.textContent.slice(0, 300));
    await check("the shell command shows", /cat hello\.txt/.test(t1?.textContent ?? ""), t1?.textContent.slice(0, 300));
    await check("there is an answer", $$(":scope > .cl-md", t1).length > 0);
    await check("context usage shows", !!$(".nt-ring"), $(".nt-ring")?.getAttribute("aria-label"));
    await shot("r1-first-turn");

    // A project command (.claude/commands/pong.md in the work folder), picked from the / menu.
    const ta = $(".nt-compose textarea");
    ta.focus();
    setValue(ta, "/pon");
    const pong = await until("/pong in the menu", () => $$(".nt-ac[data-mode=slash] .it .nm").find((x) => x.textContent === "/pong"), 30000);
    await check("the / menu lists the agent's commands, the project's own among them", !!pong, $(".nt-ac")?.textContent.slice(0, 300));
    await shot("r3-slash-menu");
    ta.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    await check("Tab puts the command in the message", !!(await until("chosen", () => ta.value === "/pong ", 3000)), ta.value);
    const before = $$(".cl-turn").length;
    $(".nt-compose .round.send").click();
    await until("a new turn", () => $$(".cl-turn").length > before, 30000);
    await idle(180000);
    await check("the agent ran the command", /SKILLPONG/.test(lastTurn()?.textContent ?? ""), lastTurn()?.textContent.slice(-200));

    // An image, pasted into the composer (or picked, where the web view will not take a constructed paste).
    const canvas = document.createElement("canvas");
    canvas.width = 256; canvas.height = 256;
    const g = canvas.getContext("2d");
    g.fillStyle = "#e01010"; g.fillRect(0, 0, 256, 256);
    const blob = await new Promise((r) => canvas.toBlob(r, "image/png"));
    const dt = new DataTransfer();
    dt.items.add(new File([blob], "red.png", { type: "image/png" }));
    let how = "paste";
    try { ta.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true })); } catch {}
    if (!(await until("pasted chip", () => $(".nt-atts img"), 2000))) {
      how = "file picker";
      const picker = $(".nt-compose input[type=file]");
      picker.files = dt.files;
      picker.dispatchEvent(new Event("change", { bubbles: true }));
    }
    await check(`the image becomes a chip (${how})`, !!(await until("chip", () => $(".nt-atts img"))));
    setValue(ta, "What single colour fills this image? Answer with one word.");
    await sleep(100);
    $(".nt-compose .round.send").click();
    await check("the sent message shows its image", !!(await until("bubble", () => $$(".nt-user:not(.nt-sending)").find((u) => $(".imgs img", u)), 30000)));
    await idle(180000);
    await check("the agent saw the image", /red|红/i.test($$(":scope > .cl-md", lastTurn()).map((r) => r.textContent).join(" ")), lastTurn()?.textContent.slice(-200));
    await check("nothing is left on its way", !$(".nt-sending"), $(".nt-sending")?.textContent);
    await shot("r4-image");

    // Steer: a long task, changed while it runs.
    await send("Count from 1 to 20 slowly: for each number N run the shell command `sleep 2 && echo N` on its own, wait for it to finish, then run the next one. Never batch or parallelize them. Then say done.");
    await until("running", running, 30000);
    const twoEchoes = await until("two commands", () => commands(lastTurn()) >= 2, 180000);
    await check("the long task runs commands", !!twoEchoes);
    await check("it is still running after two numbers", running(), `running ${running()}, commands so far ${commands(lastTurn())}`);
    await check("one send, no switch; the field says it can be messaged while it works", !$(".nt-compose .chip") && /工作时也可以继续发消息|while it works/.test($(".nt-compose textarea").placeholder), $(".nt-compose textarea").placeholder);
    await send("Change of plan: stop at 5 instead of 20.");
    await check("the message joins the running turn as a plain user message", !!(await until("steer row", () => $$(".nt-user[data-steer]", lastTurn()).some((u) => u.textContent.includes("stop at 5") && !u.querySelector(".tag")), 60000)));
    await idle(180000);
    const ran = commands(lastTurn());
    await check("the agent stopped near 5, not 20", ran >= 2 && ran <= 8, `${ran} commands; ${lastTurn()?.textContent.slice(-200)}`);
    await shot("r2-steer");
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
