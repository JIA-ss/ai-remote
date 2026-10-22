// Drives the desktop app's agent chat panel end to end, inside the real app (run by
// tools/desktop-chat-e2e.py through the debug build's AI_REMOTE_SMOKE_SCRIPT). The harness replaces
// __CONFIG__ with {work}. Every check is a `[page e2e]` line on the app's stderr; screenshots go
// through the smoke hook (`log_from_page('shot', name)`).
(async () => {
  const CFG = __CONFIG__;
  // The test window is off screen (the page is hidden, requestAnimationFrame never fires); the app
  // folds events on a timer then, which these runs rely on.
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

  // Diagnostics: say when the panel of the same session is replaced (a remount loses the composer's
  // state). Switching tabs or sessions replaces it on purpose and is not counted.
  let panel = null, panelKey = "";
  const sessionKey = () => `${$(".nt-chat-hd .cwd")?.textContent ?? ""}|${$(".nt-user")?.textContent ?? ""}`;
  setInterval(() => {
    const now = $(".nt-chat");
    if (!now) { panel = null; return; }
    const key = sessionKey();
    if (panel && now !== panel && key === panelKey && key !== "|") say("e2e", "     (note: the chat panel of the same session was remounted)");
    panel = now;
    panelKey = key;
  }, 50);
  try {
    // 0. A remembered session that is gone from the host (it restarted): the panel goes back to the
    // sessions and says so, instead of staying on an error with a composer that cannot send.
    await until("chat panel", () => $(".nt-chat-start, .nt-chat"), 30000);
    const tabs = () => $$(".nt-rail .seg [role=tab]");
    localStorage.setItem("ai-remote.chat.last", JSON.stringify({ ...JSON.parse(localStorage.getItem("ai-remote.chat.last") ?? "{}"), [CFG.host]: "c0ffee0000000" }));
    tabs()[0].click();
    await sleep(300);
    tabs()[1].click();
    const back = await until("back to the sessions", () => $(".nt-chat-start") && $(".nt-chat-picker .nt-chat-note.warn"), 20000);
    await check("a remembered session that is gone returns to the sessions with a note",
      !!back && /不在这台电脑上|no longer on the computer/.test(back.textContent),
      $(".nt-chat-status, .nt-chat-note")?.textContent);
    await check("and is forgotten", !(JSON.parse(localStorage.getItem("ai-remote.chat.last") ?? "{}")[CFG.host]));
    await shot("00-gone");

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

    // 2b. The composer's input side (one send button, / and @ menus, images, find, quote, usage,
    // messages on their way) and a sub-agent's own steps. Screenshots go to /tmp/chat-input too.
    const ta = composer();
    await check("one send button: no add-to-turn or queue switch", !$(".nt-compose .chip") && $$(".nt-compose .round.send").length === 1);
    ta.focus();
    setValue(ta, "/re");
    const slash = await until("slash menu", () => $(".nt-ac[data-mode=slash]"));
    const slashItems = $$(".nt-ac .it .nm").map((x) => x.textContent);
    await check("/ opens the agent's commands, with descriptions", !!slash && slashItems.includes("/review") && slashItems.includes("/release-notes")
      && /Review the changes/.test(slash.textContent), slashItems.join(" | "));
    setValue(ta, "/");
    await until("all commands", () => $$(".nt-ac .it").length >= 3);
    await check("skills are listed under their own heading", $$(".nt-ac .gh").map((g) => g.textContent).join("|").match(/Skills|技能/) !== null
      && $$(".nt-ac .it .nm").some((x) => x.textContent === "/tidy"), $(".nt-ac")?.textContent);
    await shot("10-slash-menu");
    setValue(ta, "/rev");
    await until("filtered", () => $$(".nt-ac .it").length === 1);
    keyDown(ta, "Tab");
    await check("Tab puts the command in the message", !!(await until("chosen command", () => composer().value === "/review ", 3000)), composer().value);
    setValue(ta, "look at @mai");
    const mention = await until("mention menu", () => $$(".nt-ac[data-mode=mention] .it .nm").find((x) => x.textContent === "src/main.rs"));
    await check("@ lists the session folder's files from the host", !!mention, $(".nt-ac")?.textContent);
    await shot("11-mention-menu");
    keyDown(ta, "Enter");
    await check("Enter puts the file's path in the message", !!(await until("chosen file", () => composer().value === "look at @src/main.rs ", 3000)), composer().value);
    setValue(ta, "");
    // An image, pasted (or picked, where the web view will not take a constructed paste).
    const canvas = document.createElement("canvas");
    canvas.width = 320; canvas.height = 180;
    const g = canvas.getContext("2d");
    g.fillStyle = "#2f80ed"; g.fillRect(0, 0, 320, 180); g.fillStyle = "#fff"; g.font = "48px sans-serif"; g.fillText("e2e", 110, 110);
    const blob = await new Promise((r) => canvas.toBlob(r, "image/png"));
    const file = new File([blob], "screenshot.png", { type: "image/png" });
    const dt = new DataTransfer();
    dt.items.add(file);
    let pasted = "paste";
    try {
      ta.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }));
    } catch {}
    if (!(await until("pasted chip", () => $(".nt-atts img"), 2000))) {
      pasted = "file picker";
      const picker = $(".nt-compose input[type=file]");
      picker.files = dt.files;
      picker.dispatchEvent(new Event("change", { bubbles: true }));
    }
    await check(`an image becomes a chip in the composer (${pasted})`, !!(await until("image chip", () => $(".nt-atts img"))));
    setValue(ta, "describe this");
    await shot("12-image-chip");
    $(".nt-compose .round.send").click();
    const sentImg = await until("user bubble with the image", () => $$(".nt-user:not(.nt-sending)").find((u) => u.textContent.includes("describe this") && $(".imgs img", u)));
    await check("the sent message shows its image", !!sentImg);
    const imgReply = await until("image reply", () => lastTurn().textContent.match(/\[images: image\/png (\d+)\]/));
    await check("the image reaches the agent", !!imgReply, lastTurn()?.textContent.slice(-200));
    await check("nothing is left on its way once delivered", !!(await until("delivered", () => !$(".nt-sending"), 5000)), $(".nt-sending")?.textContent);
    await idle();
    await shot("13-image-sent");
    // A sub-agent's own steps fold under its row.
    await send("AGENT find where the quarter end is built");
    await idle();
    const sub = await until("sub-agent row", () => $(".nt-sub", lastTurn()));
    await check("the sub-agent row counts its own steps", !!sub && /2/.test($(".steps", sub)?.textContent ?? ""), sub?.textContent);
    $(".rh", sub).click();
    await check("opening it shows the steps, then its summary", !!(await until("sub steps", () => $$(".nt-sub-steps .nt-tl", sub).length === 2 && $(".nt-sub-bd", sub))),
      sub?.textContent);
    await check("the sub-agent's steps are not in the turn's runs", !$$(".nt-run", lastTurn()).some((r) => /quarter_end/.test(r.textContent)));
    await shot("14-subagent");
    // Find (⌘F inside the panel).
    ta.focus();
    ta.dispatchEvent(new KeyboardEvent("keydown", { key: "f", metaKey: true, bubbles: true, cancelable: true }));
    const fq = await until("find bar", () => $(".nt-findbar input"));
    setValue(fq, "quarter");
    const count = await until("matches", () => /^\d+\/\d+$/.test($(".nt-findbar .ct")?.textContent ?? "") && $(".nt-findbar .ct").textContent);
    await check("⌘F finds in the conversation", !!count && parseInt(count.split("/")[1], 10) >= 3, $(".nt-findbar .ct")?.textContent);
    keyDown(fq, "Enter");
    await check("Enter goes to the next match", (await until("next", () => $(".nt-findbar .ct").textContent.startsWith("2/"), 3000)) !== null, $(".nt-findbar .ct")?.textContent);
    await shot("15-find");
    keyDown(fq, "Escape");
    await check("Esc closes the find bar", !!(await until("closed", () => !$(".nt-findbar"), 3000)));
    // Quote a selection from a reply.
    const para = $$(".nt-reply p", t1).find((p) => p.textContent.length > 10);
    const range = document.createRange();
    range.selectNodeContents(para);
    getSelection().removeAllRanges();
    getSelection().addRange(range);
    $(".nt-chat-body").dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
    const qb = await until("quote button", () => $(".nt-quote"));
    await check("a selection offers Quote", !!qb);
    await shot("16-quote");
    qb.click();
    await check("Quote puts it in the composer as a quote", !!(await until("quoted", () => composer().value.startsWith("> "), 3000)), composer().value.slice(0, 80));
    setValue(composer(), "");
    // The context breakdown.
    $("button.nt-ring").click();
    const usage = await until("usage popover", () => $(".nt-usage"));
    await check("the context ring opens a breakdown of what the agent reported", !!usage && $$(".nt-usage .urow").length >= 4, usage?.textContent);
    await shot("17-usage");
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    await until("usage closed", () => !$(".nt-usage"), 3000);

    // 3. Steer: while a turn runs, Send adds to it.
    const before = turns().length;
    await send("SLOW job");
    await check("a slow turn runs, with stop", !!(await until("running", running)));
    await sleep(2500);
    const tm = $(".nt-working .tm")?.textContent ?? "";
    await check("the working line's timer counts while hidden", /^\d+(\.\d+)? s$/.test(tm.trim()) && parseFloat(tm) >= 1, tm);
    await check("there is one send, no add-to-turn / queue switch", !$(".nt-compose .chip") && /工作时也可以继续发消息|while it works/.test(composer().placeholder), composer().placeholder);
    await send("change of plan");
    const steered = await until("steer row", () => $$(".nt-user", lastTurn()).find((u) => u.textContent.includes("change of plan")));
    await check("the message joins the running turn, unmarked like any user message", !!steered && steered.dataset.steer === "1" && !$(".tag", steered), steered?.outerHTML.slice(0, 120));
    await check("the steered turn ends with the agent's answer", !!(await until("steered answer", () => lastTurn().textContent.includes("steered: change of plan"))));
    await idle();
    await check("it adds no turn of its own", turns().length === before + 1, `${turns().length} vs ${before + 1}`);
    await shot("02-steer");

    // 4. Stop and continue.
    await send("SLOW again");
    await until("running", running);
    $(".nt-compose .round.stop").click();
    const stopped = await until("stopped", () => $(".nt-stopped", lastTurn()));
    await check("stop ends the turn and offers to continue", !!stopped);
    await shot("04-stopped");
    const cont = await until("continue button", () => $(".nt-stopped .btn"));
    cont.click();
    await check("continue sends a new turn", !!(await until("continue turn", () => !$(".nt-stopped") && lastTurn().querySelector(".nt-user"))));
    await idle();

    // 5. An agent that cannot take a message mid-turn: Send waits for the turn to end; a waiting
    // message can be taken back.
    $$(".nt-chat-hd .ico").at(-1).click();
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
    await check("its composer says the message waits for the turn, no switch", !$(".nt-compose .chip") && /本轮结束后|current turn ends/.test(composer().placeholder), composer().placeholder);
    await send("take me back");
    const queued = await until("queued", () => $$(".nt-qd").find((q) => q.textContent.includes("take me back")));
    await check("the waiting message shows above the composer", !!queued);
    await shot("03-queued");
    const remove = await until("remove button", () => $$(".nt-qd").find((q) => q.textContent.includes("take me back"))?.querySelectorAll(".nt-ib")[1]);
    remove.click();
    await check("it can be taken back", !!(await until("dequeued", () => !$$(".nt-qd").some((q) => q.textContent.includes("take me back")))));
    await send("late message");
    await check("another waits in the queue", !!(await until("queued", () => $$(".nt-qd").some((q) => q.textContent.includes("late message")))));
    $(".nt-compose .round.stop").click();
    await check("after stop the waiting message runs as the next turn", !!(await until("queued turn", () => lastTurn().textContent.includes("you said: late message"), 20000)));
    await idle();
    await check("the taken-back message never ran", !$$(".nt-user").some((u) => u.textContent.includes("take me back")));
    await shot("05-no-steer");

    // 6. Back to the first session: the transcript replays.
    $$(".nt-chat-hd .ico").at(-1).click();
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
