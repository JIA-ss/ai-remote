// The desktop chat panel with a real agent (tools/desktop-chat-e2e.py --real <name>): a small file task,
// then a steer while it works. Same hooks as desktop-chat-e2e.js.
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
  const running = () => !!$(".nt-compose .round.stop");
  const lastTurn = () => $$(".nt-turn").at(-1);
  // Commands of a turn: runs fold them into "运行了 5 条命令" / "Ran 5 commands"; single calls are tool lines.
  const commands = (turn) => {
    if (!turn) return 0;
    let n = 0;
    for (const s of $$(".nt-run .sent", turn)) for (const m of s.textContent.matchAll(/(?:运行了?|[Rr]an|[Rr]unning) ?(\d+) ?(?:条命令|commands?)/g)) n += Number(m[1]);
    return n + $$(".nt-tl.execute, .nt-tl", turn).filter((x) => !x.closest(".nt-run") && $(".mono", x)).length;
  };
  async function send(text) {
    const ta = await until("composer", () => $(".nt-compose textarea"));
    setValue(ta, text);
    await sleep(100);
    $(".nt-compose .round.send").click();
  }
  const idle = (ms) => until("turn end", () => !running() && !$(".nt-working") && lastTurn()?.querySelector(".nt-foot, .nt-stopped"), ms);

  try {
    await until("start form", () => $(".nt-chat-start"), 30000);
    const pick = $$(".nt-chat-pick .opt").find((b) => b.querySelector(".nm")?.textContent === CFG.agent);
    await check(`${CFG.agent} is offered`, !!pick, $$(".nt-chat-pick .opt").map((b) => b.textContent).join(" | "));
    pick.click();
    setValue($("#ag-cwd"), CFG.work);
    setValue($("#ag-prompt"), "Create a file named hello.txt containing the single line: hi from ai-remote. Then run `cat hello.txt` in the shell. Answer with one short sentence.");
    await sleep(200);
    $(".nt-chat-start .btn.primary").click();
    await check("the real agent starts", !!(await until("conversation", () => $(".nt-chat .nt-chat-hd"), 60000)));
    await check("its first turn finishes", !!(await idle(180000)));
    const t1 = lastTurn();
    await check("the turn completed (not failed)", !$(".nt-notice.error", t1) && !$(".nt-stopped", t1), t1?.textContent.slice(-300));
    const edit = $(".nt-diff", t1) || $(".nt-run", t1) || $(".nt-tl", t1);
    await check("the agent's tool calls show (edit card, run or tool line)", !!edit, t1?.textContent.slice(0, 300));
    await check("the shell command shows", /cat hello\.txt/.test(t1?.textContent ?? ""), t1?.textContent.slice(0, 300));
    await check("there is an answer", $$(".nt-reply", t1).length > 0);
    await check("context usage shows", !!$(".nt-ring"), $(".nt-ring")?.getAttribute("aria-label"));
    await shot("r1-first-turn");

    // Steer: a long task, changed while it runs.
    await send("Count from 1 to 20 slowly: for each number N run the shell command `sleep 2 && echo N` on its own, wait for it to finish, then run the next one. Never batch or parallelize them. Then say done.");
    await until("running", running, 30000);
    const twoEchoes = await until("two commands", () => commands(lastTurn()) >= 2, 180000);
    await check("the long task runs commands", !!twoEchoes);
    await check("it is still running after two numbers", running(), `running ${running()}, commands so far ${commands(lastTurn())}`);
    await check("the composer defaults to steering", !!$(".nt-compose .chip.steer"), $(".nt-compose .chip")?.className);
    await send("Change of plan: stop at 5 instead of 20.");
    await check("the steer joins the running turn, marked", !!(await until("steer row", () => $$(".nt-user.steer", lastTurn()).some((u) => u.textContent.includes("stop at 5")), 60000)));
    await idle(180000);
    const ran = commands(lastTurn());
    await check("the agent stopped near 5, not 20", ran >= 2 && ran <= 8, `${ran} commands; ${lastTurn()?.textContent.slice(-200)}`);
    await shot("r2-steer");
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
