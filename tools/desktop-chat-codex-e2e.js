// Drives the Codex view of the desktop app's agent chat panel end to end, inside the real app, against
// "Codex fake" (tools/fake-codex-app-server.py, which speaks Codex's app-server protocol through the
// host's real Codex adapter). Run with:
//   E2E_SCRIPT=tools/desktop-chat-codex-e2e.js python3 tools/desktop-chat-e2e.py
// Covers: a full turn (live line naming what runs, plan, folding behind "Worked for" once the answer
// streams, the answer, the turn's changes), opening the work (groups, commands without their shell,
// read / search / list lines, a failed command's shell with its exit code, diffs with line numbers),
// a question, a steer and Stop / Continue, a provider failure, many files and long output, a host that
// sends no phases, a compaction, one file edited twice (the turn's net diff), an edit that does not
// apply, phases that arrive late, find, Claude's session keeping its own rows, and the replay.
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
  const keyDown = (el, key, opts = {}) => el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, ...opts }));
  const composer = () => $(".nt-compose textarea");
  const running = () => !!$(".nt-compose .round.stop");
  const turns = () => $$(".cdx-turn");
  const lastTurn = () => turns().at(-1);
  const text = (el) => (el?.textContent ?? "").replace(/\s+/g, " ").trim();
  const lines = (root) => $$(".cdx-act > .x-hd > .x-ln", root).map(text);
  const folderShown = () => $(".nt-chat-start .nt-folder-btn .nt-folder-lbl .fn")?.textContent ?? "";
  async function send(t) {
    const ta = await until("composer", composer);
    setValue(ta, t);
    await sleep(100);
    $(".nt-compose .round.send").click();
    await until("composer cleared", () => composer().value === "", 5000);
  }
  /** Sends a message that starts a turn, and waits for that turn to show. */
  async function sendTurn(t) {
    const n = turns().length;
    await send(t);
    await until("the new turn", () => turns().length > n, 15000);
  }
  const idle = (ms = 30000) => until("turn end", () => !running() && lastTurn() && !lastTurn().classList.contains("running"), ms);
  const openWork = async (turn) => {
    const tg = $(".cdx-worked .x-tg", turn);
    if (tg && tg.getAttribute("aria-expanded") !== "true") tg.click();
    return until("work shown", () => $(".cdx-work", turn), 3000);
  };
  const openLine = async (line) => {
    const ln = $(":scope > .x-hd > .x-ln", line);
    if (ln && ln.getAttribute("aria-expanded") !== "true") ln.click();
    await sleep(250);
    return line;
  };
  const WORKED = /Worked for \d+(m \d+)?s|工作了 \d+( 分 \d+)? 秒/;

  try {
    // 1. Start Codex fake from the start form, in the work folder.
    await until("chat panel", () => $(".nt-chat-start, .nt-chat"), 30000);
    if ($(".nt-chat")) {
      $$(".nt-chat-hd .ico").at(-1).click();
      (await until("session menu", () => $$(".nt-chat-menu [role=menuitem]")[0])).click();
    }
    const form = await until("start form", () => $(".nt-chat-start"), 30000);
    const pick = $$(".nt-chat-pick .opt").find((b) => b.textContent.includes("Codex fake"));
    await check("the start form offers the Codex stand-in", !!form && !!pick, $$(".nt-chat-pick .opt").map(text).join(" | "));
    pick.click();
    const work = CFG.work.split(/[\\/]/).pop();
    const above = CFG.work.slice(0, CFG.work.length - work.length - 1);
    if (folderShown() !== work) {
      $(".nt-chat-start .nt-folder-btn").click();
      const q = await until("folder field", () => $("#ag-folder-q"));
      setValue(q, above);
      await until("open row", () => $(".nt-folder-list .row.open"));
      keyDown(q, "Enter");
      const child = await until("work folder", () => $$(".nt-folder-list .row.child").find((r) => r.querySelector(".fn")?.textContent === work));
      child.click();
      await until("crumbs", () => $(".nt-folder-crumbs .cur")?.textContent === work);
      (await until("use button", () => $(".nt-folder-ft .btn.primary:not(:disabled)"))).click();
      await until("chosen", () => folderShown() === work);
    }
    await check("the work folder is chosen", folderShown() === work, folderShown());
    setValue($("#ag-prompt"), "EXPLORE fix the quarter export");
    await sleep(200);
    $(".nt-chat-start .btn.primary").click();
    const panel = await until("conversation", () => $(".nt-chat .nt-chat-hd"), 30000);
    await check("starting opens the conversation", !!panel);
    await check("a Codex session gets the Codex view", !!(await until("codex view", () => $(".nt-chat.cdx"))), $(".nt-chat")?.className);
    await check("its header names the agent", /Codex fake/.test(text($(".nt-chat-hd .nm"))), text($(".nt-chat-hd .nm")));

    // 2. While it works: the user's bubble, a live line naming what runs (commands without their
    // shell), "Working for", the plan open; then the work folds behind "Worked for" as the answer streams.
    const bubble = await until("user bubble", () => $$(".cdx-turn .nt-user .bubble").find((b) => b.textContent.includes("EXPLORE fix the quarter export")));
    await check("the message shows in a bubble on the right", !!bubble && getComputedStyle(bubble.parentElement).alignItems === "flex-end", bubble?.parentElement && getComputedStyle(bubble.parentElement).alignItems);
    const live = new Set();
    let workingSeen = false, foldedWhileStreaming = false, thinkingSeen = false;
    const planSeen = new Set();
    // The answer streams for well under a second: look at every change, not only at each poll (a
    // window in the background gets its timers slowed down).
    const foldWatch = new MutationObserver(() => {
      const t = lastTurn();
      if (t && $(".cdx-answer.streaming", t) && $(".cdx-worked", t) && !$(".cdx-work", t)) foldedWhileStreaming = true;
    });
    foldWatch.observe($(".nt-chat"), { subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ["class"] });
    const watchEnd = Date.now() + 40000;
    while (Date.now() < watchEnd) {
      const t = lastTurn();
      if (t) {
        for (const s of $$(".cdx-shimmer", t)) live.add(text(s));
        if (/Working|已工作|工作中/.test(text($(".cdx-status", t)))) workingSeen = true;
        if ($(".cdx-thinking", t)) thinkingSeen = true;
        const pill = $(".cdx-plan .x-pill", t);
        if (pill && $(".cdx-plan ol", t)) planSeen.add(text(pill));
        if ($(".cdx-answer.streaming", t) && $(".cdx-worked", t) && !$(".cdx-work", t)) foldedWhileStreaming = true;
        if (!t.classList.contains("running") && !running()) break;
      }
      await sleep(60);
    }
    foldWatch.disconnect();
    const liveList = [...live];
    await check("a live line names the command running now, without its shell",
      liveList.some((l) => /(Running|正在运行) python3 export\.py --quarter Q3/.test(l)) && !liveList.some((l) => /\/bin\/zsh|-lc/.test(l)), liveList.join(" | "));
    await check("reads, searches and listings show as what they do while they run",
      liveList.some((l) => /(Searching|Reading|Listing|Read|Searched|Listed|正在搜索|正在读取|正在列出|读取了|搜索了|列出了)/.test(l)), liveList.join(" | "));
    await check("'Working for' counts while the turn runs", workingSeen);
    await check("the plan is open while it runs, with its step", [...planSeen].some((p) => /Step \d \/ 3|第 \d \/ 3 步/.test(p)), [...planSeen].join(" | "));
    await check("the work folds behind 'Worked for' as soon as the answer streams", foldedWhileStreaming);
    void thinkingSeen;
    await idle();
    let t1 = turns()[0];
    await shot("c01-turn-folded");

    // 3. The finished turn: "Worked for", the answer, the plan folded and done, the turn's changes.
    await check("the finished turn says how long it worked", WORKED.test(text($(".cdx-worked .x-tg", t1))), text($(".cdx-worked", t1)));
    await check("the work stays folded", !$(".cdx-work", t1));
    const answer = $(".cdx-answer", t1);
    await check("the answer renders Markdown: bold, table, code block", !!answer && !!$("strong", answer) && !!$("table", answer) && !!$(".nt-cb", answer), text(answer).slice(0, 120));
    await check("the plan folds once done and says so", !$(".cdx-plan ol", t1) && /3 out of 3|已完成 3 \/ 3/.test(text($(".cdx-plan .x-pill", t1))), text($(".cdx-plan", t1)));
    const changes = $(".cdx-changes", t1);
    await check("the turn's changes: 2 files with their counts", !!changes && /2 files changed|2 个文件已更改/.test(text($(".x-hd", changes)))
      && /\+11/.test(text($(".x-hd .cdx-stat", changes))) && /−1/.test(text($(".x-hd .cdx-stat", changes))), text($(".x-hd", changes)));
    const fileRows = $$(".x-file", changes);
    await check("each changed file is listed by its path, the new one marked", fileRows.length === 2 && text(fileRows[0]).includes("export.py")
      && text(fileRows[1]).includes("tests/test_quarters.py") && !!$(".x-tag", fileRows[1]), fileRows.map(text).join(" | "));
    $(".x-ln", fileRows[0]).click();
    const fileDiff = await until("file diff", () => $(".cdx-diff", fileRows[0]), 3000);
    await check("a changed file opens to its diff with line numbers",
      !!fileDiff && $$(".x-dl.x-add", fileDiff).length === 2 && $$(".x-dl.x-del", fileDiff).length === 1 && $$(".x-no", fileDiff).some((n) => /^\d+$/.test(n.textContent)),
      fileDiff && `${$$(".x-dl.x-add", fileDiff).length}+ ${$$(".x-dl.x-del", fileDiff).length}-`);
    await check("the answer can be copied", !!$(".cdx-foot .nt-ib", t1));

    // 4. Opening the work: the remarks, a thought, two groups summed up in a sentence.
    const w = await openWork(t1);
    await check("'Worked for' opens the work", !!w);
    const units = $$(".cdx-unit", w);
    await check("the remarks show between the steps", units.filter((u) => u.dataset.unit === "message").length === 2, units.map((u) => u.dataset.unit).join(","));
    await check("the reasoning shows as a 'Thought' line", !!$(".cdx-act.thought", w), lines(w).join(" | "));
    const groups = $$(".cdx-act.group", w);
    const sentences = groups.map((g) => text($(":scope > .x-hd", g)));
    await check("tool calls in a row fold into one line each, summed up",
      groups.length === 2 && /(Read files.*ran a command|读取了文件.*运行了命令)/.test(sentences[0]) && /(Edited files|编辑了文件)/.test(sentences[1]), sentences.join(" | "));
    await shot("c02-work-open");
    await openLine(groups[0]);
    const g1 = lines($(".cdx-list", groups[0]));
    await check("searches, reads and listings read as what they did",
      g1.some((l) => /^(Searched|搜索了) .*quarter_end/.test(l)) && g1.some((l) => /^(Read|读取了) export\.py$/.test(l)) && g1.some((l) => /^(Listed|列出了) (files|文件)$/.test(l)), g1.join(" | "));
    await check("no command shows the shell Codex wraps it in", !g1.some((l) => /zsh|-lc/.test(l)), g1.join(" | "));
    const failed = $$(".cdx-list .cdx-act", groups[0]).find((a) => /python3 export\.py --quarter Q3/.test(text(a)));
    await check("the failing command is marked with its exit code", !!failed && failed.classList.contains("failed") && /exit 1|退出码 1/.test(text(failed)), text(failed));
    await openLine(failed);
    const shell = await until("shell", () => $(".cdx-shell", failed), 3000);
    await check("a command opens to its shell: the command, its output, how it ended",
      !!shell && /\$ python3 export\.py --quarter Q3/.test(text($("pre.x-cmd", shell))) && /ValueError/.test(text($("pre.x-out", shell)))
        && /Exit code 1|退出码 1/.test(text($(".x-sh-ft", shell))) && !/exit code 1\)/.test(text($("pre.x-out", shell))),
      shell && `${text($("pre.x-cmd", shell))} | ${text($(".x-sh-ft", shell))}`);
    await openLine(groups[1]);
    const g2acts = $$(".cdx-list .cdx-act", groups[1]);
    const g2 = g2acts.map((a) => text($(":scope > .x-hd", a)));
    await check("edits read as Edited / Created with their counts",
      g2.some((l) => /^(Edited|编辑了) export\.py \+2 ?−1$/.test(l)) && g2.some((l) => /^(Created|创建了) test_quarters\.py \+9$/.test(l)), g2.join(" | "));
    await check("a passing command says how long it ran", g2.some((l) => /python3 -m pytest -q (in 2\.3s|用时 2\.3 秒)/.test(l)), g2.join(" | "));
    await check("the web search reads as one", g2.some((l) => /(Searched the web|搜索了网页) .*python calendar monthrange/.test(l)), g2.join(" | "));
    const edit = g2acts.find((a) => /export\.py/.test(text(a)) && /Edited|编辑了/.test(text(a)));
    await openLine(edit);
    const diff = await until("edit diff", () => $(".cdx-diff", edit), 3000);
    await check("an edit opens to its diff", !!diff && $$(".x-dl.x-add", diff).length === 2, diff && text(diff).slice(0, 120));
    const pass = g2acts.find((a) => /pytest/.test(text(a)));
    await openLine(pass);
    await check("a passing command's shell says Success", /Success|成功/.test(text($(".cdx-shell .x-sh-ft", pass))), text($(".cdx-shell .x-sh-ft", pass)));
    await shot("c03-lines-open");
    // Folding again keeps what was opened inside.
    $(".cdx-worked .x-tg", t1).click();
    await check("'Worked for' folds the work again", !!(await until("folded", () => !$(".cdx-work", t1), 3000)));

    // 5. A question: the turn waits for the answer, then goes on.
    await sendTurn("ASK which modules");
    const q = await until("question", () => $(".cdx .nt-q:not(.done)", lastTurn()));
    await check("the agent's question shows with its choices", !!q && $$(".opt", q).length === 2 && /monthly\.py/.test(text(q)), text(q));
    await check("the turn says it waits for the answer", /Waiting for your answer|等你回答/.test(text($(".cdx-status", lastTurn()))), text($(".cdx-status", lastTurn())));
    await shot("c04-question");
    $$(".opt", q)[0].click();
    await idle();
    await check("the answer reaches the agent and the turn ends", /Going with Fix it too/.test(text($(".cdx-answer", lastTurn()))), text(lastTurn()).slice(-160));
    await openWork(lastTurn());
    await check("the question shows answered", !!$(".nt-q.done", lastTurn()), text($(".nt-q", lastTurn())));

    // 6a. A message added to a turn that then finishes stays in view when its work folds.
    await sendTurn("STEERME build it");
    await until("live build", () => $$(".cdx-act.is-live", lastTurn()).some((a) => /npm run build/.test(text(a))));
    await send("use the production config");
    await idle();
    await check("the steered turn folds, the added message stays in view",
      !!$(".cdx-worked", lastTurn()) && !$(".cdx-work", lastTurn()) && $$(".nt-user[data-steer='1']", lastTurn()).some((u) => u.textContent.includes("use the production config"))
        && /taken into account: use the production config/.test(text($(".cdx-answer", lastTurn()))), text(lastTurn()).slice(-200));

    // 6. Steer, then Stop and Continue.
    const before = turns().length;
    await sendTurn("SLOW count");
    const sleeping = await until("live sleep", () => $$(".cdx-act.is-live", lastTurn()).find((a) => /(Running|正在运行) sleep 30/.test(text(a))));
    await check("a long command shows as running, shimmering", !!sleeping && !!$(".cdx-shimmer", sleeping), text(lastTurn()).slice(-120));
    await send("stop at three");
    const steered = await until("steer bubble", () => $$(".nt-user", lastTurn()).find((u) => u.textContent.includes("stop at three")));
    await check("a message sent meanwhile joins the running turn", !!steered && steered.dataset.steer === "1");
    await check("the agent takes it", !!(await until("noted", () => text(lastTurn()).includes("Noted: stop at three"))));
    await check("no new turn for it", turns().length === before + 1, `${turns().length} vs ${before + 1}`);
    await shot("c05-steer");
    // "Stopping…" shows from the click until the agent confirms (only a moment with the stand-in).
    let stoppingSeen = false;
    const watch = new MutationObserver(() => { if (/Stopping…|正在停止…/.test(text($(".cdx-status", lastTurn())))) stoppingSeen = true; });
    watch.observe(lastTurn(), { subtree: true, childList: true, characterData: true });
    $(".nt-compose .round.stop").click();
    if (/Stopping…|正在停止…/.test(text($(".cdx-status", lastTurn())))) stoppingSeen = true;
    const stopped = await until("stopped", () => $(".cdx-divider.x-stop", lastTurn()));
    watch.disconnect();
    await check("the status says Stopping… until the agent stops", stoppingSeen);
    await check("Stop ends the turn: 'You stopped after …'", !!stopped && /You stopped after|你在.*后停止了/.test(text(stopped)), text(stopped));
    await check("a stopped turn does not fold its work", !$(".cdx-worked", lastTurn()) && !!$(".cdx-work", lastTurn()));
    await check("the cut-short command says Stopped", $$(".cdx-act", lastTurn()).some((a) => /^(Stopped|停止了) sleep 30/.test(text($(":scope > .x-hd", a)))), lines(lastTurn()).join(" | "));
    const cont = await until("continue", () => $(".nt-stopped .btn", lastTurn()));
    await check("it offers to continue", !!cont);
    await shot("c06-stopped");
    cont.click();
    await check("Continue sends a new turn", !!(await until("continued", () => turns().length === before + 2)));
    await idle();

    // 7. The provider refuses: the turn fails with its words.
    await sendTurn("FAIL please");
    await idle();
    const failNote = $(".nt-notice.error", lastTurn());
    await check("a failed turn says why, in the provider's words", !!failNote && /The model is overloaded/.test(text(failNote)), text(lastTurn()).slice(-160));

    // 8. Many files, long output, a long diff.
    await sendTurn("MANY rename helpers");
    await idle();
    const many = $(".cdx-changes", lastTurn());
    await check("eight files changed, six listed", !!many && /8 files changed|8 个文件已更改/.test(text($(".x-hd", many))) && $$(".x-file", many).length === 6, text($(".x-hd", many)));
    const more = $(":scope > .x-more", many);
    await check("'Show 2 more files' lists the rest", !!more && /2/.test(text(more)));
    more.click();
    await check("all eight are listed then", !!(await until("all files", () => $$(".x-file", many).length === 8, 3000)));
    const constants = $$(".x-file", many).find((f) => text(f).includes("constants.py"));
    $(".x-ln", constants).click();
    const longDiff = await until("long diff", () => $(".cdx-diff", constants), 3000);
    await check("a long diff shows its first 400 lines and offers the rest", !!longDiff && $$(".x-dl", longDiff).length === 400 && /300/.test(text($(".x-more", longDiff))), longDiff && `${$$(".x-dl", longDiff).length} lines`);
    const wm = await openWork(lastTurn());
    for (const g of $$(".cdx-act.group", wm)) await openLine(g);
    const bigCmd = $$(".cdx-list .cdx-act", wm).find((a) => /print\(300 lines\)/.test(text(a)));
    await check("the long command is listed in the work", !!bigCmd, lines(wm).join(" | "));
    await openLine(bigCmd);
    const bigShell = await until("big shell", () => $(".cdx-shell", bigCmd), 3000);
    await check("long output shows its last 40 lines and offers the rest", !!bigShell && /260/.test(text($(".x-more", bigShell))) && /line 300/.test(text($("pre.x-out", bigShell))) && !/line 1:/.test(text($("pre.x-out", bigShell))),
      bigShell && text($(".x-more", bigShell)));
    $(".x-more", bigShell)?.click();
    await check("and all of it on request", !!(await until("all output", () => /line 1:/.test(text($("pre.x-out", bigShell))), 3000)));
    await shot("c07-many");

    // 9. A Codex that sends no phases: the last text of the finished turn is the answer.
    await sendTurn("NOPHASE look at the readme");
    await idle();
    await check("without phases the last message still becomes the answer, the rest folds",
      /The README describes the report tool/.test(text($(".cdx-answer", lastTurn()))) && !!$(".cdx-worked", lastTurn()), text(lastTurn()).slice(-160));

    // 10. A compaction shows as a divider in the work.
    await sendTurn("COMPACT now");
    await idle();
    await openWork(lastTurn());
    await check("a compaction shows as a divider", $$(".cdx-divider", lastTurn()).some((d) => /Context automatically compacted|上下文已自动压缩/.test(text(d))), text(lastTurn()).slice(-160));

    // 10b. One file edited twice, a scratch file created then deleted: the changes are the turn's net diff.
    await sendTurn("REEDIT the readme");
    await idle();
    const ch = $(".cdx-changes", lastTurn());
    await check("edits of one file count once, by the turn's net diff; a scratch file is gone",
      !!ch && $$(".x-file", ch).length === 1 && text($(".x-file", ch)).includes("README.md") && /\+4/.test(text($(".x-file", ch))) && !/−/.test(text($(".x-file", ch))),
      ch && text(ch));
    const wr = await openWork(lastTurn());
    for (const g of $$(".cdx-act.group", wr)) await openLine(g);
    const rl = $$(".cdx-act:not(.group) > .x-hd > .x-ln", wr).map(text);
    await check("the work keeps every edit, the scratch file created and deleted",
      rl.filter((l) => /^(Edited|编辑了) README\.md/.test(l)).length === 2 && rl.some((l) => /^(Created|创建了) scratch\.txt/.test(l)) && rl.some((l) => /^(Deleted|删除了) scratch\.txt/.test(l)),
      rl.join(" | "));

    // 10c. An edit that does not apply.
    await sendTurn("BADPATCH rename");
    await idle();
    await check("a failed edit changes nothing", !$(".cdx-changes", lastTurn()));
    const wb = await openWork(lastTurn());
    const bad = $$(".cdx-act", wb).find((a) => /^(Failed to edit|未能编辑) export\.py$/.test(text($(":scope > .x-hd > .x-ln", a))));
    await check("it reads 'Failed to edit', marked, without counts", !!bad && bad.classList.contains("failed"), lines(wb).join(" | "));

    // 10d. Phases that only arrive as the messages complete: the answer is the answer, once.
    await sendTurn("LATEPHASE readme");
    await idle();
    await openWork(lastTurn());
    const lateAnswer = text($(".cdx-answer", lastTurn()));
    await check("a late phase still makes the answer, shown once",
      /LATE ANSWER/.test(lateAnswer) && text(lastTurn()).split("LATE ANSWER").length === 2 && /Checking the README first/.test(text($(".cdx-work", lastTurn()))),
      text(lastTurn()).slice(-240));
    const codexTurns = turns().length;

    // 11. Find works in the Codex view.
    keyDown($(".nt-chat"), "f", { metaKey: true, ctrlKey: navigator.platform.startsWith("Win") });
    const field = await until("find field", () => $(".nt-findbar input"));
    setValue(field, "quarter");
    const count = await until("find count", () => /^\d+\/\d+$/.test($(".nt-findbar .ct")?.textContent ?? "") && $(".nt-findbar .ct").textContent);
    await check("⌘F finds in the Codex view", !!count && parseInt(count.split("/")[1], 10) >= 2, count);
    keyDown(field, "Escape");
    await until("find closed", () => !$(".nt-findbar"), 3000);
    await shot("c08-later-turns");

    // 12. A Claude Code session keeps its own rows.
    $$(".nt-chat-hd .ico").at(-1).click();
    (await until("session menu", () => $$(".nt-chat-menu [role=menuitem]")[0])).click();
    await until("picker", () => $(".nt-chat-start"));
    $$(".nt-chat-pick .opt").find((b) => b.textContent.includes("Rich fake")).click();
    setValue($("#ag-prompt"), "RICH check the export");
    await sleep(200);
    $(".nt-chat-start .btn.primary").click();
    await until("claude conversation", () => $(".nt-chat .nt-chat-hd") && /Rich fake/.test(text($(".nt-chat-hd .nm"))), 30000);
    await until("claude turn", () => $(".nt-turn .nt-foot, .nt-turn .nt-stopped"), 30000);
    await check("a Claude Code session keeps the panel's own rows, not Codex's", !$(".nt-chat.cdx") && !!$(".nt-turn") && !!$(".nt-run") && !$(".cdx-turn"),
      $(".nt-chat")?.className);
    await shot("c09-claude");

    // 13. Back to the Codex session: it replays the same way.
    $$(".nt-chat-hd .ico").at(-1).click();
    (await until("session menu", () => $$(".nt-chat-menu [role=menuitem]")[0])).click();
    await until("sessions", () => $$(".nt-sessions .sess").length >= 2);
    $$(".nt-sessions .sess").find((s) => s.textContent.includes("Codex fake")).click();
    const replayed = await until("replay", () => $(".nt-chat.cdx") && turns().length === codexTurns && turns()[0].querySelector(".cdx-worked"), 30000);
    await check("reopening the Codex session replays every turn in the Codex view", !!replayed, `${turns().length} of ${codexTurns} turns`);
    t1 = turns()[0];
    await until("first turn folded", () => !$(".cdx-work", t1), 3000);
    await check("the first turn is folded again, with its changes", WORKED.test(text($(".cdx-worked", t1))) && !$(".cdx-work", t1) && !!$(".cdx-changes", t1));
    const rre = turns().find((t) => /REEDIT/.test(text($(".nt-user", t))));
    await check("the replayed net diff is the same", !!rre && /\+4/.test(text($(".cdx-changes .x-file", rre))) && $$(".cdx-changes .x-file", rre).length === 1, rre && text($(".cdx-changes", rre)));
    const w1 = await openWork(t1);
    await openLine($$(".cdx-act.group", w1)[0]);
    const again = lines($(".cdx-list", $$(".cdx-act.group", w1)[0]));
    await check("the replayed lines read the same", again.some((l) => /^(Read|读取了) export\.py$/.test(l)) && again.some((l) => /exit 1|退出码 1/.test(l)), again.join(" | "));
    await shot("c10-replay");
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
