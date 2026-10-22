// Drives the desktop app's agent chat panel end to end, inside the real app (run by
// tools/desktop-chat-e2e.py through the debug build's AI_REMOTE_SMOKE_SCRIPT). The harness replaces
// __CONFIG__ with {work}. Every check is a `[page e2e]` line on the app's stderr; screenshots go
// through the smoke hook (`log_from_page('shot', name)`).
(async () => {
  const CFG = __CONFIG__;
  // The test window is off screen, so the page is hidden and requestAnimationFrame never fires; the panel
  // folds incoming events once per frame. A timer stands in for frames while hidden.
  if (document.visibilityState === "hidden") {
    window.requestAnimationFrame = (cb) => setTimeout(() => cb(performance.now()), 16);
    window.cancelAnimationFrame = (id) => clearTimeout(id);
  }
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
  // React-controlled fields: set through the native setter so React sees the change.
  function setValue(el, v) {
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, "value").set.call(el, v);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  }
  const composer = () => $(".nt-compose textarea");
  const running = () => !!$(".nt-compose .round.stop");
  const turns = () => $$(".nt-turn");
  const lastTurn = () => turns().at(-1);
  // The folder picker (ui/chat/FolderPicker.tsx).
  const keyDown = (el, key) => el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
  const folderShown = () => $(".nt-chat-start .nt-folder-btn .nt-folder-lbl .fn")?.textContent ?? "";
  async function send(text) {
    const ta = await until("composer", composer);
    setValue(ta, text);
    await sleep(100);
    $(".nt-compose .round.send").click();
    await until("composer cleared", () => composer().value === "", 5000);
  }
  const idle = (ms = 20000) => until("turn end", () => !running() && !$(".nt-working") && lastTurn()?.querySelector(".nt-foot, .nt-stopped"), ms);

  try {
    // 1. The start form: pick the rich stand-in, a folder and a first message.
    const form = await until("start form", () => $(".nt-chat-start"), 30000);
    await check("the chat panel offers to start an agent", !!form);
    const pick = $$(".nt-chat-pick .opt").find((b) => b.textContent.includes("Rich fake"));
    await check("chat_runtimes lists the stand-in agents", !!pick && $$(".nt-chat-pick .opt").some((b) => b.textContent.includes("No-steer fake")),
      $$(".nt-chat-pick .opt").map((b) => b.textContent).join(" | "));
    pick.click();
    // 1a. The folder: with nothing recent the picker opens on the host's home (files_list over the real
    // channel); a typed path is browsed, a subfolder entered with a click, and the footer takes it.
    const work = CFG.work.split("/").pop();
    const above = CFG.work.slice(0, CFG.work.length - work.length - 1);
    await check("with no folder chosen the field asks for one", !!$(".nt-chat-start .nt-folder-lbl.none"), $(".nt-chat-start .nt-folder-btn")?.textContent);
    $(".nt-chat-start .nt-folder-btn").click();
    const home = await until("home listing", () => $$(".nt-folder-list .row.child").length > 0 && $(".nt-folder-crumbs"));
    await check("the picker opens on the host's home and lists its folders", !!home && !$(".nt-folder-note.err"),
      `${$(".nt-folder-crumbs")?.textContent} | ${$$(".nt-folder-list .row.child").length} folders | ${$(".nt-folder-note")?.textContent ?? ""}`);
    await check("hidden folders are left out", !$$(".nt-folder-list .row.child .fn").some((x) => x.textContent.startsWith(".")));
    const q = $("#ag-folder-q");
    setValue(q, above);
    await check("a typed path offers to open it", !!(await until("open row", () => $(".nt-folder-list .row.open"))));
    keyDown(q, "Enter");
    const child = await until("work folder", () => $$(".nt-folder-list .row.child").find((r) => r.textContent.trim() === work));
    await check("the typed path is browsed and lists the work folder", !!child, $$(".nt-folder-list .row").map((r) => r.textContent).join(" | "));
    await shot("00-folder-browse");
    child.click();
    await check("entering a folder updates the crumbs", !!(await until("crumbs", () => $(".nt-folder-crumbs .cur")?.textContent === work)), $(".nt-folder-crumbs")?.textContent);
    const use = await until("use button", () => $(".nt-folder-ft .btn.primary:not(:disabled)"));
    await check("the footer offers this folder by name", !!use && use.textContent.includes(work), use?.textContent);
    use.click();
    await check("the chosen folder shows by name, the full path on hover",
      !!(await until("chosen", () => folderShown() === work)) && $(".nt-chat-start .nt-folder-btn .nt-folder-lbl").title === CFG.work,
      `${folderShown()} | ${$(".nt-chat-start .nt-folder-btn .nt-folder-lbl")?.title}`);
    setValue($("#ag-prompt"), "RICH fix the quarter export");
    await sleep(200);
    $(".nt-chat-start .btn.primary").click();
    await check("starting opens the conversation", !!(await until("conversation", () => $(".nt-chat .nt-chat-hd"), 30000)));
    const hdFolder = await until("header folder", () => $(".nt-chat-hd .nt-folder-lbl"));
    await check("the header shows the folder's name and where it is", !!hdFolder && $(".fn", hdFolder)?.textContent === work
      && hdFolder.title === CFG.work && ($(".fp", hdFolder)?.textContent ?? "").length > 0, hdFolder?.textContent);
    await check("the first message shows as the user's", !!(await until("user row", () => $$(".nt-user").some((u) => u.textContent.includes("RICH fix the quarter export")))));
    await check("the first turn finishes", !!(await idle(30000)));

    // 2. What the turn shows: reasoning, a plan, a folded run, an edit, Markdown, context usage.
    const t1 = lastTurn();
    await check("reasoning folds into its own row", !!$(".nt-reason", t1));
    const plan = $(".nt-plan", t1);
    await check("the plan shows 3 entries, all done", plan && $$("li", plan).length === 3 && $$("li.completed", plan).length === 3, plan?.textContent);
    const run = $(".nt-run", t1);
    await check("tool calls fold into one run with a sentence", !!run && $(".sent", run).textContent.length > 0, run?.textContent);
    await check("the run counts the failed command", !!run && /1/.test($(".cnt", run)?.textContent ?? ""), run?.querySelector(".cnt")?.textContent);
    await check("the run sums up the edit", !!run && /\+2/.test($(".stat", run)?.textContent ?? "") && /1/.test($(".stat .del", run)?.textContent ?? ""), run?.querySelector(".stat")?.textContent);
    $(".rh", run).click();
    await until("run opened", () => $$(".nt-tl", run).length >= 3);
    await check("an opened run lists every call", $$(".nt-tl", run).length >= 3 && !!$(".nt-diff", run), $$(".nt-tl", run).map((x) => x.textContent).join(" | "));
    const bad = $(".nt-tl.failed", run);
    $(".rh", bad).click();
    const out = await until("failed output", () => $(".nt-out.failed pre", bad));
    await check("a failed command opens to its command and red output", !!out && out.textContent.includes("exit code 1") && !!$(".nt-cmd", bad), out?.textContent);
    $(".nt-diff .rh", run).click();
    const diff = await until("diff rows", () => $(".nt-diff-rows", run));
    await check("the edit opens to a diff with added and removed lines", !!diff && $$(".ln.add", diff).length === 2 && $$(".ln.del", diff).length === 1,
      diff?.textContent);
    const reply = $$(".nt-reply", t1).at(-1);
    await check("the answer renders Markdown: heading, list, table, code block",
      !!reply && !!$("h1, h2, h3, h4, h5, h6", reply) && !!$("li", reply) && !!$("table", reply) && !!$("pre", reply), reply?.innerHTML.slice(0, 200));
    const ring = $(".nt-ring");
    await check("context usage shows as a ring", !!ring && /4[12]k \/ 200k/.test(ring.getAttribute("aria-label") ?? ""), ring?.getAttribute("aria-label"));
    await shot("01-first-turn");

    // 3. Steer: while a turn runs, Send adds to it.
    const before = turns().length;
    await send("SLOW job");
    await check("a slow turn runs, with stop", !!(await until("running", running)));
    await check("an agent that can steer defaults to steering", !!$(".nt-compose .chip.steer"), $(".nt-compose .chip")?.className);
    await send("change of plan");
    await check("the steer joins the running turn, marked", !!(await until("steer row", () => $$(".nt-user.steer", lastTurn()).some((u) => u.textContent.includes("change of plan")))));
    await check("the steered turn ends with the agent's answer", !!(await until("steered answer", () => lastTurn().textContent.includes("steered: change of plan"))));
    await idle();
    await check("a steer adds no turn of its own", turns().length === before + 1, `${turns().length} vs ${before + 1}`);
    await shot("02-steer");

    // 4. Queue, take it back, stop, continue.
    await send("SLOW again");
    await until("running", running);
    $(".nt-compose .chip.steer").click();
    await check("the chip switches to queueing", !!(await until("queue chip", () => $(".nt-compose .chip.queue"))));
    await send("take me back");
    const queued = await until("queued", () => $$(".nt-qd").find((q) => q.textContent.includes("take me back")));
    await check("the queued message shows above the composer", !!queued);
    await shot("03-queued");
    $$(".nt-ib", queued).at(-1).click();
    await check("dequeue takes it back", !!(await until("dequeued", () => !$$(".nt-qd").some((q) => q.textContent.includes("take me back")))));
    $(".nt-compose .round.stop").click();
    const stopped = await until("stopped", () => $(".nt-stopped", lastTurn()));
    await check("stop ends the turn and offers to continue", !!stopped);
    await sleep(800);
    await check("the taken-back message never ran", !$$(".nt-user").some((u) => u.textContent.includes("take me back")));
    await shot("04-stopped");
    const cont = await until("continue button", () => $(".nt-stopped .btn"));
    await check("the continue button is still there after the screenshot", !!cont);
    cont.click();
    await check("continue sends a new turn", !!(await until("continue turn", () => !$(".nt-stopped") && lastTurn().querySelector(".nt-user"))));
    await idle();

    // 5. An agent that cannot steer: Send only queues; the queued message runs after Stop.
    $(".nt-chat-hd .ico").click();
    (await until("session menu", () => $$(".nt-chat-menu [role=menuitem]")[0])).click();
    await until("picker", () => $(".nt-chat-start"));
    await check("the session list shows the first session", $$(".nt-sessions .sess").length >= 1, $$(".nt-sessions .sess").map((s) => s.textContent).join(" | "));
    $$(".nt-chat-pick .opt").find((b) => b.textContent.includes("No-steer fake")).click();
    await check("the last folder is chosen already", folderShown() === work, folderShown());
    $(".nt-chat-start .nt-folder-btn").click();
    const recent = await until("recent list", () => $(".nt-folder-list .row.recent"));
    await check("the picker opens on recent folders, the chosen one marked", !!recent && !!$(".nt-folder-list .row.recent.sel")
      && $$(".nt-folder-list .row.recent").length === 1, $$(".nt-folder-list .row").map((r) => r.textContent).join(" | "));
    await check("the session list groups sessions under their folder", $$(".nt-sessions .nt-proj-hd .fn").some((x) => x.textContent === work));
    await shot("04b-folder-recent");
    recent.click();
    await until("picker closed", () => !$(".nt-folder-pop"), 5000);
    setValue($("#ag-prompt"), "SLOW job");
    await sleep(200);
    $(".nt-chat-start .btn.primary").click();
    await check("the second agent starts and runs", !!(await until("running", running, 30000)));
    await check("an agent without steering only queues", !!$(".nt-compose .chip.queue.static") && !$(".nt-compose .chip.steer"), $(".nt-compose .chip")?.className);
    await send("late message");
    await check("its message waits in the queue", !!(await until("queued", () => $$(".nt-qd").some((q) => q.textContent.includes("late message")))));
    $(".nt-compose .round.stop").click();
    await check("after stop the queued message runs as the next turn", !!(await until("queued turn", () => lastTurn().textContent.includes("you said: late message"), 20000)));
    await idle();
    await shot("05-no-steer");

    // 6. Back to the first session: the transcript replays.
    $(".nt-chat-hd .ico").click();
    (await until("session menu", () => $$(".nt-chat-menu [role=menuitem]")[0])).click();
    await until("picker", () => $$(".nt-sessions .sess").length >= 2);
    await check("the session list has both sessions", $$(".nt-sessions .sess").length === 2);
    $$(".nt-sessions .sess").find((s) => s.textContent.includes("Rich fake")).click();
    await check("reopening replays the conversation", !!(await until("replay", () => turns().length >= 4 && document.body.textContent.includes("steered: change of plan"))), `${turns().length} turns`);
    await shot("06-replay");
  } catch (e) {
    await check("no script errors", false, e && e.stack || e);
  }
  // Leave nothing behind in the webview storage (shared with the installed app): this run's host only.
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
