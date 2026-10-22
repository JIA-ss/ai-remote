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
    setValue($("#ag-cwd"), CFG.work);
    setValue($("#ag-prompt"), "RICH fix the quarter export");
    await sleep(200);
    $(".nt-chat-start .btn.primary").click();
    await check("starting opens the conversation", !!(await until("conversation", () => $(".nt-chat .nt-chat-hd"), 30000)));
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
    $(".nt-chat-hd .ico").click();
    (await until("session menu", () => $$(".nt-chat-menu [role=menuitem]")[0])).click();
    await until("picker", () => $(".nt-chat-start"));
    await check("the session list shows the first session", $$(".nt-sessions .sess").length >= 1, $$(".nt-sessions .sess").map((s) => s.textContent).join(" | "));
    $$(".nt-chat-pick .opt").find((b) => b.textContent.includes("No-steer fake")).click();
    setValue($("#ag-cwd"), CFG.work);
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
