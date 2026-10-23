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
  let failures = 0, checks = 0;
  const check = (name, ok, detail = "") => {
    checks++;
    if (!ok) failures++;
    return say("e2e", `${ok ? "ok  " : "FAIL"}  ${name}${detail ? `  (${String(detail).slice(0, 300)})` : ""}`);
  };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const $ = (sel, root = document) => root?.querySelector(sel) ?? null;
  const $$ = (sel, root = document) => root ? Array.from(root.querySelectorAll(sel)) : [];
  async function until(what, fn, ms = 20000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      try {
        const v = fn();
        if (v) return v;
      } catch {}
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
  /** Send the second Esc as soon as React has committed the first press, within the real 2 s window. */
  function escapeTwiceWithinWindow(root, press, isArmed) {
    return new Promise((resolve, reject) => {
      if (isArmed()) {
        reject(new Error("Esc pair started while the previous press was still armed"));
        return;
      }
      let firstAt = performance.now();
      let done = false;
      const finish = (error, timing) => {
        if (done) return;
        done = true;
        observer.disconnect();
        clearTimeout(timer);
        if (error) reject(error);
        else resolve(timing);
      };
      const onArmed = () => {
        if (done) return;
        try {
          if (!isArmed()) return;
          const secondAt = performance.now();
          const timing = { firstAt, secondAt, elapsedMs: secondAt - firstAt, armedBeforeSecond: true };
          if (timing.elapsedMs >= 2000) {
            finish(new Error(`Esc armed too late for the 2 s pair: ${JSON.stringify(timing)}`));
            return;
          }
          press();
          finish(null, timing);
        } catch (error) {
          finish(error);
        }
      };
      const observer = new MutationObserver(onArmed);
      const timer = setTimeout(() => finish(new Error(`Esc did not arm within 2000 ms: ${JSON.stringify({
        elapsedMs: performance.now() - firstAt, armed: isArmed(),
      })}`)), 2000);
      observer.observe(root, { childList: true, subtree: true, characterData: true });
      try {
        firstAt = performance.now();
        press();
        onArmed();
      } catch (error) {
        finish(error);
      }
    });
  }
  const scrollGeometry = (el) => ({ scrollTop: el.scrollTop, clientHeight: el.clientHeight, scrollHeight: el.scrollHeight });
  /** Wait for the browser to deliver the scroll before resizing; assigning scrollTop alone is not an event. */
  function scrollToObserved(el, target, ms = 5000) {
    return new Promise((resolve, reject) => {
      const before = scrollGeometry(el);
      const started = performance.now();
      let events = 0;
      const finish = (error, observed) => {
        el.removeEventListener("scroll", onScroll);
        clearTimeout(timer);
        if (error) reject(error);
        else resolve({ before, target, observed, elapsedMs: performance.now() - started, events });
      };
      const onScroll = (event) => {
        if (!event.isTrusted || event.target !== el) return;
        events++;
        if (Math.abs(el.scrollTop - target) <= 1) finish(null, scrollGeometry(el));
      };
      const timer = setTimeout(() => finish(new Error(`browser did not deliver the target scroll within ${ms} ms: ${JSON.stringify({
        before, target, actual: scrollGeometry(el), events,
      })}`)), ms);
      el.addEventListener("scroll", onScroll);
      el.scrollTop = target;
    });
  }
  /** Subscribe before changing the composer, then read geometry after the resize delivery round. */
  function resizeObserved(el, change, ms = 5000) {
    return new Promise((resolve, reject) => {
      const before = scrollGeometry(el);
      const started = performance.now();
      let done = false, deliveryTimer = null, notifications = 0;
      const finish = (error, notified) => {
        if (done) return;
        done = true;
        observer.disconnect();
        clearTimeout(timer);
        clearTimeout(deliveryTimer);
        if (error) reject(error);
        else resolve({ before, notified, after: scrollGeometry(el), elapsedMs: performance.now() - started, notifications });
      };
      const observer = new ResizeObserver((entries) => {
        if (done || !entries.some((entry) => entry.target === el)) return;
        notifications++;
        // The initial observation at the old height is not the resize under test.
        if (el.clientHeight === before.clientHeight || deliveryTimer !== null) return;
        const notified = scrollGeometry(el);
        // A task boundary lets the product's observers finish this delivery round. Do not wait for
        // any expected scroll delta: the assertion after this helper must still catch a wrong one.
        deliveryTimer = setTimeout(() => finish(null, notified), 0);
      });
      const timer = setTimeout(() => finish(new Error(`browser did not deliver a changed body height within ${ms} ms: ${JSON.stringify({
        before, actual: scrollGeometry(el), notifications,
      })}`)), ms);
      observer.observe(el);
      try {
        change();
      } catch (error) {
        finish(error);
      }
    });
  }
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
  const detailedNavigationState = () => ({ ...navigationState(), url: window.location.href,
    turnCount: turns().length, recentTurns: turns().slice(-3).map((turn) => ({ key: turn.dataset.turn, connected: turn.isConnected, className: turn.className })) });
  // Workspace navigation: the real New action and session rows, with no recovery clicks.
  const workspacePanel = () => $(".ws[data-embedded=true]") ?? $(".ws");
  const selectedSession = () => $(".ws-row[data-selected]", workspacePanel())?.dataset.rowKey ?? null;
  const navigationState = () => ({
    route: $(".win")?.dataset.route ?? null,
    chat: $(".nt-chat")?.className ?? null,
    form: !!$(".ws-new", workspacePanel()),
    sessions: $$(".ws-row", workspacePanel()).length,
    selected: selectedSession(),
    sidebar: workspacePanel()?.dataset.side ?? null,
    menu: !!$(".ws-menu"),
    hidden: document.hidden,
    focus: document.activeElement?.className ?? null,
  });
  async function workspaceSidebar() {
    const pane = workspacePanel();
    if (!pane?.isConnected) throw new Error("workspace is missing or disconnected");
    if (!$(".ws-side", pane)) {
      const toggle = $(".ws-corner .ws-ib", pane);
      if (!toggle?.isConnected) throw new Error("workspace sidebar toggle is missing or disconnected");
      toggle.click();
    }
    const sidebar = await until("workspace session sidebar", () => $(".ws-side", workspacePanel()), 5000);
    if (!sidebar) throw new Error("workspace sidebar did not open on its first toggle click");
    return sidebar;
  }
  async function leaveSession(what, ready, options = {}) {
    const state = options.state ?? navigationState;
    const before = state();
    await workspaceSidebar();
    const button = $(".ws-side .ws-nav", workspacePanel());
    if (!button?.isConnected) {
      await options.report?.("navigation-failure", { before, reason: "New action missing or disconnected" });
      throw new Error(`${what}: workspace New action is missing or disconnected`);
    }
    button.click();
    const left = await until("conversation left for workspace New", () => $(".ws-new", workspacePanel()) && !$(".ws-session .nt-chat", workspacePanel()), 8000);
    const evidence = { before, after: state() };
    await check(`${what} leaves the conversation on the first New click`, !!left);
    if (!left) {
      await say("e2e", `     navigation failure: ${JSON.stringify(evidence)}`);
      await options.report?.("navigation-failure", evidence);
      throw new Error(`${what}: the first New click did not leave the conversation`);
    }
    if (!ready()) await say("e2e", `     waiting for ${what} after leaving: ${JSON.stringify(state())}`);
    const loaded = await until(`${what}: workspace content ready`, ready, 30000);
    await check(`${what} content loads after leaving the conversation`, !!loaded);
    if (!loaded) {
      await say("e2e", `     workspace loading failure: ${JSON.stringify(state())}`);
      await options.report?.("workspace-loading-failure", { before, after: state() });
      throw new Error(`${what}: left the conversation but its content did not load within 30 s`);
    }
  }
  async function openWorkspaceSession(key, what = "the session reopens") {
    await workspaceSidebar();
    const row = $$(".ws-row", workspacePanel()).find((candidate) => candidate.dataset.rowKey === key);
    const button = $(".ws-row-main", row);
    if (!row?.isConnected || !button?.isConnected) throw new Error(`${what}: the exact session row is missing or disconnected: ${key}`);
    button.click();
    const opened = await until("the selected workspace conversation", () => selectedSession() === key && $(".ws-session .nt-chat", workspacePanel()), 30000);
    await check(`${what} opens on its first row click`, !!opened);
    if (!opened) throw new Error(`${what}: the first row click did not open ${key}; ${JSON.stringify(navigationState())}`);
    return opened;
  }
  // Workspace startup: reuse its existing CLI chip, project picker, model and first-message composer.
  const folderShown = () => text($(".ws-chip-folder .nt-folder-lbl .fn"));
  const folderPath = () => $(".ws-chip-folder .nt-folder-lbl")?.title ?? "";
  async function prepareWorkspace() {
    const pane = await until("complete workspace", workspacePanel, 30000);
    if (!pane) throw new Error("the complete workspace did not appear");
    const divider = $(".nt-live-divider");
    if (divider) {
      // Dock the existing sidebar so its selected row is visible throughout these data-flow checks.
      for (let i = 0; i < 12; i++) { keyDown(divider, "ArrowLeft"); await sleep(20); }
      if (!(await until("workspace sidebar docked", () => workspacePanel()?.dataset.side === "docked", 5000)))
        throw new Error("workspace did not have room for its existing session sidebar");
    }
    if ($(".ws-session .nt-chat", pane)) await leaveSession("the new-session page opens", () => $(".ws-new"));
    if (!(await until("workspace new-session composer", () => $(".ws-new .ws-composer textarea"), 30000)))
      throw new Error("the workspace new-session composer did not appear");
  }
  async function pickAgent(name) {
    const chip = await until("workspace CLI selector", () => $(".ws-env-trail .ws-chip"));
    if (!chip?.isConnected) throw new Error("workspace CLI selector is missing");
    if (text($(".ws-chip-text", chip)) === name) return true;
    chip.click();
    const item = await until(`workspace CLI ${name}`, () => $$(".ws-menu .ws-menu-item").find((choice) => text($(".ws-menu-label", choice)) === name), 5000);
    if (!item?.isConnected) return false;
    item.click();
    return !!(await until("workspace CLI chosen", () => text($(".ws-env-trail .ws-chip .ws-chip-text")) === name, 5000));
  }
  async function typeFolder(path) {
    const name = path.split(/[\\/]/).filter(Boolean).pop();
    if (folderShown() === name && folderPath() === path) return;
    $(".ws-chip-folder .nt-folder-btn").click();
    const field = await until("folder search box", () => $("#ag-folder-q"));
    if (!field) throw new Error("workspace folder picker did not open");
    setValue(field, path);
    await until("open-path row", () => $(".nt-folder-list .row.open"));
    keyDown(field, "Enter");
    const use = await until("folder listed", () => text($(".nt-folder-crumbs .cur")) === name && $(".nt-folder-crumbs .cur")?.title === path && $(".nt-folder-ft .btn.primary:not(:disabled)"));
    if (!use) throw new Error(`workspace folder picker did not list ${path}`);
    use.click();
    if (!(await until("folder chosen", () => folderShown() === name && folderPath() === path && !$(".nt-folder-pop"), 5000)))
      throw new Error(`workspace did not keep the chosen folder ${path}`);
  }
  const sceneError = (reason, detail = {}) => new Error(`${reason}; ${JSON.stringify({ ...detail, scene: detailedNavigationState() })}`);
  const sameConversation = (panel, turn) => !!panel?.isConnected && panel === $(".nt-chat.cdx")
    && !!turn?.isConnected && turn === lastTurn();
  async function screenshotAt(name, target) {
    const feed = $(".nt-chat-body");
    if (!feed || !target) throw new Error(`${name}: screenshot target or transcript missing`);
    feed.dispatchEvent(new WheelEvent("wheel", { deltaY: -10, bubbles: true }));
    target.scrollIntoView({ block: "start", inline: "nearest", behavior: "auto" });
    const visible = await until(`${name} screenshot target in view`, () => {
      const rect = target.getBoundingClientRect(), view = feed.getBoundingClientRect();
      return target.isConnected && rect.width > 0 && rect.height > 0 && rect.bottom > view.top && rect.top < view.bottom
        && rect.right > view.left && rect.left < view.right;
    }, 5000);
    const rect = target.getBoundingClientRect(), view = feed.getBoundingClientRect();
    const detail = JSON.stringify({ target: { top: rect.top, bottom: rect.bottom }, feed: { top: view.top, bottom: view.bottom } });
    await check(`${name} shows its intended target`, !!visible, detail);
    if (!visible) throw new Error(`${name}: screenshot target stayed outside the transcript: ${detail}`);
    await shot(name);
  }
  const lines = (root) => $$(".cdx-act > .x-hd > .x-ln", root).map(text);
  async function send(t) {
    const panel = $(".nt-chat.cdx");
    const ta = await until("composer", composer);
    if (!ta?.isConnected || !panel?.isConnected) throw sceneError("send: no connected Codex composer", { message: t });
    setValue(ta, t);
    const button = await until("message ready to send", () => $(".nt-compose .round.send:not(:disabled)"));
    if (!button?.isConnected || button.disabled) throw sceneError("send: Send never became connected and enabled", { message: t });
    if (panel !== $(".nt-chat.cdx") || ta !== composer()) throw sceneError("send: conversation remounted before Send", { message: t });
    button.click();
    const cleared = await until("composer cleared", () => composer()?.value === "", 5000);
    if (!cleared) throw sceneError("send: the submitted message never cleared", { message: t });
  }
  /** Sends a message that starts a turn, and waits for that turn to show. */
  async function sendTurn(t) {
    const n = turns().length;
    await send(t);
    const turn = await until("the new turn", () => {
      const latest = lastTurn();
      return turns().length > n && latest?.isConnected && text($(".nt-user", latest)).includes(t) && latest;
    }, 15000);
    if (!turn) throw sceneError("sendTurn: no new connected turn contains the submitted message", { message: t, previousTurns: n });
    return turn;
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
  const WORKED = /Worked for (\d+[dhms] ?)+|用时 (\d+(天|小时|分钟|秒) ?)+/;

  try {
    // 1. Start Codex from the complete workspace's existing CLI and project controls.
    await prepareWorkspace();
    await check("the workspace offers the Codex stand-in", await pickAgent("Codex fake"));
    await typeFolder(CFG.work);
    const work = CFG.work.split(/[\\/]/).pop();
    await check("the work folder is chosen", folderShown() === work && folderPath() === CFG.work, `${folderShown()} | ${folderPath()}`);
    setValue($(".ws-new .ws-composer textarea"), "EXPLORE fix the quarter export");
    const startButton = await until("first message ready", () => $(".ws-new .ws-send:not(:disabled)"), 5000);
    if (!startButton) throw new Error("the first Codex message did not become ready");
    // Observe before starting: the host can already be running a short command while the form's
    // start response and the runtime-specific conversation are still mounting.
    const live = new Set();
    const liveWatch = new MutationObserver(() => {
      const turn = lastTurn();
      if (turn) for (const line of $$(".cdx-act.is-live .x-tx .cdx-cad > .x-base", turn)) live.add(text(line));
    });
    liveWatch.observe(document.body, { subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ["class"] });
    startButton.click();
    const panel = await until("conversation", () => $(".ws-session .nt-chat.cdx"), 30000);
    const codexSession = selectedSession();
    await check("starting opens the conversation", !!panel);
    await check("a Codex session gets the Codex view", !!(await until("codex view", () => $(".nt-chat.cdx"))), $(".nt-chat")?.className);
    await check("its header names the agent", /Codex fake/.test(text($(".nt-chat-hd .nm"))), text($(".nt-chat-hd .nm")));

    // 2. While it works: the user's bubble, a live line naming what runs (commands without their
    // shell), "Working for", the plan open; then the work folds behind "Worked for" as the answer streams.
    const bubble = await until("user bubble", () => $$(".cdx-turn .nt-user .bubble").find((b) => b.textContent.includes("EXPLORE fix the quarter export")));
    await check("the message shows in a bubble on the right", !!bubble && getComputedStyle(bubble.parentElement).alignItems === "flex-end", bubble?.parentElement && getComputedStyle(bubble.parentElement).alignItems);
    let workingSeen = false, foldedWhileStreaming = false, titleSeen = false, sweepSeen = false, rawSeen = false, dividerFirst = null, thoughtRow = false;
    const planSeen = new Set();
    let planSteps = 0;
    // The answer streams for well under a second, and the reasoning's title shows only until the next
    // step: look at every change, not only at each poll (a window in the background gets its timers
    // slowed down).
    const foldWatch = new MutationObserver(() => {
      const t = lastTurn();
      if (!t) return;
      if ($(".cdx-answer.streaming", t) && $(".cdx-worked", t) && !$(".cdx-work", t)) foldedWhileStreaming = true;
      if (text($(".cdx-thinking", t)) === "Reading the export code") titleSeen = true;
      if ($("[data-sweep]", t)) sweepSeen = true;
      if (/RAW REASONING/.test(t.textContent)) rawSeen = true;
      if ($(".cdx-act.thought", t)) thoughtRow = true;
      const wk = $(".cdx-worked", t), wo = $(".cdx-work", t);
      if (wk && wo && dividerFirst === null) dividerFirst = !!(wk.compareDocumentPosition(wo) & Node.DOCUMENT_POSITION_FOLLOWING);
      const pill = $(".cdx-live .cdx-plan-pill");
      if (pill) {
        planSeen.add(text($(":scope > .tnum", pill)));
        planSteps = Math.max(planSteps, $$(".cdx-plan-pop li", pill).length);
      }
    });
    foldWatch.observe($(".nt-chat"), { subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ["class"] });
    const watchEnd = Date.now() + 40000;
    while (Date.now() < watchEnd) {
      const t = lastTurn();
      if (t) {
        for (const s of $$(".cdx-act.is-live .x-tx .cdx-cad > .x-base", t)) live.add(text(s));
        if (/Working|处理中|已处理/.test(text($(".cdx-worked", t)))) workingSeen = true;
        const pill = $(".cdx-live .cdx-plan-pill");
        if (pill && !planSteps) {
          pill.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
          pill.focus();
        }
        if ($(".cdx-answer.streaming", t) && $(".cdx-worked", t) && !$(".cdx-work", t)) foldedWhileStreaming = true;
        if (!t.classList.contains("running") && !running()) break;
      }
      await sleep(60);
    }
    foldWatch.disconnect();
    liveWatch.disconnect();
    const liveList = [...live];
    await check("a live line names the command running now, without its shell",
      liveList.some((l) => /(Running|正在运行) python3 export\.py --quarter Q3/.test(l)) && !liveList.some((l) => /\/bin\/zsh|-lc/.test(l)), liveList.join(" | "));
    await check("reads, searches and listings show as what they do while they run",
      liveList.some((l) => /(Searching|Reading|Listing|Read|Searched|Listed|正在搜索|正在读取|正在列出|读取了|搜索了|列出了)/.test(l)), liveList.join(" | "));
    await check("'Working for' counts while the turn runs", workingSeen);
    await check("the plan shows as a pill above the composer, with its step", [...planSeen].some((p) => /Step \d \/ 3|第 \d \/ 3 步/.test(p)), [...planSeen].join(" | "));
    await check("the pill lists the plan's steps", planSteps === 3, `${planSteps} steps`);
    await check("'Working for' sits at the top of the work", dividerFirst === true, String(dividerFirst));
    await check("the reasoning's title says what the agent is at", titleSeen);
    await check("reasoning is never a line of the work, nor its raw text shown", !thoughtRow && !rawSeen, `${thoughtRow} ${rawSeen}`);
    const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;
    await check("the live text respects the system's motion setting", reducedMotion ? !sweepSeen : sweepSeen,
      `reduced motion: ${reducedMotion}; sweep: ${sweepSeen}`);
    await check("the work folds behind 'Worked for' as soon as the answer streams", foldedWhileStreaming);
    await idle();
    let t1 = turns()[0];
    await shot("c01-turn-folded");

    // 3. The finished turn: "Worked for" (held still), the answer, no plan, the turn's changes.
    const worked1 = text($(".cdx-worked .x-tg", t1));
    await check("the finished turn says how long it worked", WORKED.test(worked1), worked1);
    await sleep(1500);
    await check("and that time stays put", text($(".cdx-worked .x-tg", t1)) === worked1, text($(".cdx-worked .x-tg", t1)));
    await check("the work stays folded", !$(".cdx-work", t1));
    const answer = $(".cdx-answer", t1);
    await check("the answer renders Markdown: bold, table, code block", !!answer && !!$("strong", answer) && !!$("table", answer) && !!$(".nt-cb", answer), text(answer).slice(0, 120));
    await check("the plan leaves with the turn: no pill, nothing in the transcript", !$(".cdx-live") && !$(".cdx-plan, .cdx-plan-pill", t1));
    const changes = $(".cdx-changes", t1);
    await check("the turn's changes: 'Edited 2 files' with their counts, and Review", !!changes && /Edited 2 files|已编辑 2 个文件/.test(text($(".x-hd .x-ti", changes)))
      && /\+11/.test(text($(".x-hd .cdx-stat", changes))) && /−1/.test(text($(".x-hd .cdx-stat", changes))) && /^(Review|审阅)$/.test(text($(".x-hd .btn", changes))),
      text($(".x-hd", changes)));
    const fileRows = $$(".x-file", changes);
    await check("each changed file is listed by its path, the new one marked", fileRows.length === 2 && text(fileRows[0]).includes("export.py")
      && text(fileRows[1]).includes("tests/test_quarters.py") && !!$(".x-dot.x-new", fileRows[1]) && !$(".x-dot", fileRows[0]), fileRows.map(text).join(" | "));
    $(".x-ln", fileRows[0]).click();
    const fileDiff = await until("file diff", () => $(".cdx-diff", fileRows[0]), 3000);
    await check("a changed file opens to its diff with line numbers",
      !!fileDiff && $$(".x-dl.x-add", fileDiff).length === 2 && $$(".x-dl.x-del", fileDiff).length === 1 && $$(".x-no", fileDiff).some((n) => /^\d+$/.test(n.textContent)),
      fileDiff && `${$$(".x-dl.x-add", fileDiff).length}+ ${$$(".x-dl.x-del", fileDiff).length}-`);
    await check("the answer can be copied, and says when it came", !!$(".cdx-foot .nt-ib", t1) && /\d{1,2}:\d{2}/.test(text($(".cdx-foot .x-meta", t1))) && !/tokens/.test(text($(".cdx-foot", t1))),
      text($(".cdx-foot", t1)));
    $(".x-hd .btn", changes).click();
    await check("Review opens every file's diff", !!(await until("all diffs", () => $$(".x-file .cdx-diff", changes).length === 2, 3000)));
    $(".x-hd .btn", changes).click();
    await check("and closes them all again", !!(await until("diffs closed", () => $$(".x-file .cdx-diff", changes).length === 0, 3000)));

    // 4. Opening the work: the remarks and two groups summed up in a sentence.
    const w = await openWork(t1);
    await check("'Worked for' opens the work", !!w);
    const units = $$(".cdx-unit", w);
    await check("the remarks show between the steps", units.filter((u) => u.dataset.unit === "message").length === 2, units.map((u) => u.dataset.unit).join(","));
    await check("no line for the reasoning in the work", !$(".cdx-act.thought", w) && !/RAW REASONING|Locating the date math/.test(w.textContent), lines(w).join(" | "));
    const groups = $$(".cdx-act.group", w);
    const sentences = groups.map((g) => text($(":scope > .x-hd", g)));
    await check("tool calls in a row fold into one line each, summed up",
      groups.length === 2 && /(Read files.*ran a command|读取了文件.*运行了命令)/.test(sentences[0]) && /(Edited files|编辑了文件)/.test(sentences[1]), sentences.join(" | "));
    await screenshotAt("c02-work-open", w);
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
    await check("a passing command says how long it ran", g2.some((l) => /python3 -m pytest -q (in 2s|用时 2秒)/.test(l)), g2.join(" | "));
    await check("the web search reads as one", g2.some((l) => /(Searched the web|搜索了网页): python calendar monthrange/.test(l)), g2.join(" | "));
    const edit = g2acts.find((a) => /export\.py/.test(text(a)) && /Edited|编辑了/.test(text(a)));
    await openLine(edit);
    const diff = await until("edit diff", () => $(".cdx-diff", edit), 3000);
    await check("an edit opens to its diff", !!diff && $$(".x-dl.x-add", diff).length === 2, diff && text(diff).slice(0, 120));
    const pass = g2acts.find((a) => /pytest/.test(text(a)));
    await openLine(pass);
    await check("a passing command's shell says Success", /Success|成功/.test(text($(".cdx-shell .x-sh-ft", pass))), text($(".cdx-shell .x-sh-ft", pass)));
    await screenshotAt("c03-lines-open", groups[0]);
    // Folding again keeps what was opened inside.
    $(".cdx-worked .x-tg", t1).click();
    await check("'Worked for' folds the work again", !!(await until("folded", () => !$(".cdx-work", t1), 3000)));

    // 5. Questions: a card in place of the composer, one question at a time; the answers go together.
    // The reader is still in the composer when the question comes: the card must not take the keys
    // they type next.
    composer().focus();
    await sendTurn("ASK which modules");
    const card = await until("question card", () => $(".cdx-reqs .cdx-req"));
    await check("a question that comes while one types does not take the keyboard", !card.contains(document.activeElement), document.activeElement?.className);
    await check("the question comes as a card in place of the composer", !!card && $(".nt-compose-slot").hidden && !$(".nt-q", lastTurn()),
      `${!!card} ${$(".nt-compose-slot")?.hidden}`);
    await check("its title is the question, without its header; 1 / 2", /Should I fix report\/monthly\.py too\?/.test(text($(".x-ti", card))) && !/Scope/.test(text($(".x-hd", card)))
      && /1 \/ 2/.test(text($(".x-pager", card))), text($(".x-hd", card)));
    const opts = $$(".x-opt[role=radio]", card);
    await check("numbered options, the first chosen, with descriptions and the Recommended mark",
      opts.length === 2 && opts[0].getAttribute("aria-checked") === "true" && /Same bug, same fix/.test(text(opts[0])) && /^1/.test(text($(".x-mk", opts[0])))
        && text($(".x-badge", opts[0])) !== "" && !/\(Recommended\)/.test(text(opts[0])), opts.map(text).join(" | "));
    await check("a row for an answer of one's own, with Skip", !!$(".x-other textarea", card) && /Skip|跳过/.test(text($(".x-other .btn", card))));
    await check("the turn says it waits for the answer", /Waiting for your answer|等你回答/.test(text($(".cdx-thinking.x-ask", lastTurn()))), text(lastTurn()).slice(-80));
    await sleep(500);
    await check("the card's options are drawn (faded in, in view)", getComputedStyle($(".x-body", card)).opacity === "1" && opts[0].getBoundingClientRect().height >= 32,
      `${getComputedStyle($(".x-body", card)).opacity} ${opts[0].getBoundingClientRect().height}`);
    await shot("c04-question");
    card.dispatchEvent(new KeyboardEvent("keydown", { key: "1", bubbles: true, cancelable: true }));
    await check("a number key takes that option and moves on", !!(await until("second question", () => /2 \/ 2/.test(text($(".cdx-req .x-pager"))), 3000)));
    const card2 = $(".cdx-reqs .cdx-req");
    await check("the second question has no row of its own, Skip below", /Add a test for every quarter\?/.test(text($(".x-ti", card2))) && !$(".x-other", card2) && /Skip|跳过/.test(text($(".x-acts .btn", card2))));
    $(".x-acts .btn", card2).focus();
    $(".x-acts .btn", card2).click();
    await idle();
    await check("after answering, the keyboard is back in the composer", document.activeElement === composer(), document.activeElement?.className);
    await check("the answers reach the agent together, the skipped one left out",
      /Going with Fix it too \(Recommended\); tests: no answer\. Answered: scope\./.test(text($(".cdx-answer", lastTurn()))), text($(".cdx-answer", lastTurn())));
    await check("the composer is back", !$(".cdx-reqs") && !$(".nt-compose-slot").hidden);
    const wq = await openWork(lastTurn());
    const asked = $$(".cdx-act.asked", wq)[0];
    await check("the request shows as one 'Asked' line", !!asked && /^(Asked 2 questions|已询问 2 个问题)$/.test(text($(":scope > .x-hd > .x-ln", asked))), lines(wq).join(" | "));
    await openLine(asked);
    const qa = $$(".cdx-asked .x-qa", asked).map(text);
    await check("opening it shows each answer, and none for the skipped one", qa[0] === "Fix it too (Recommended)" && /No answer provided|未提供答案/.test(qa[1]), qa.join(" | "));

    // 5b. Closing the card stops the turn; what was typed in the composer is still there.
    await sendTurn("ASK again");
    // A draft in the composer (hidden behind the card, still there).
    setValue(composer(), "my unsent draft");
    const card3 = await until("question card again", () => $(".cdx-reqs .cdx-req"), 15000);
    // Enter on a button elsewhere in the panel is that button's, not an answer.
    const tg = $(".cdx-worked button.x-tg", turns()[0]);
    tg.focus();
    tg.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    await sleep(400);
    await check("Enter on another button does not answer the question", !!$(".cdx-reqs .cdx-req") && /1 \/ 2/.test(text($(".cdx-req .x-pager"))));
    card3.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    await check("Esc closes the card at once", !!(await until("card gone", () => !$(".cdx-reqs"), 3000)));
    await idle();
    await check("and stops the turn: Codex waits on the answer", /stopped after|停止了/.test(text($(".cdx-worked", lastTurn()))) || lastTurn().querySelector(".cdx-worked.x-stop") !== null,
      text(lastTurn()).slice(-160));
    await check("the composer keeps its draft", composer().value === "my unsent draft", composer().value);
    setValue(composer(), "");

    // 5c. A question Codex closes itself: the card goes, the turn goes on.
    await sendTurn("RESOLVED name it");
    const card4 = await until("free-text card", () => $(".cdx-reqs .cdx-req"), 15000);
    await check("a question without options asks for text", !!card4 && !!$(".x-free textarea", card4) && /What should the report be called\?/.test(text($(".x-ti", card4))));
    await check("one that does not block says when Codex goes on by itself", /(Codex goes on without an answer in|后 Codex 会不等回答继续)/.test(text($(".x-expires", card4)))
      && /[0-9]:[0-5][0-9]/.test(text($(".x-expires", card4))), text($(".x-expires", card4)));
    await check("Codex closing it takes the card away", !!(await until("card gone", () => !$(".cdx-reqs"), 8000)));
    await idle();
    await check("the turn goes on to its answer", /kept the name/.test(text($(".cdx-answer", lastTurn()))), text(lastTurn()).slice(-160));

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
    await check("a long command shows as running, its text the live kind", !!sleeping && !!$(".cdx-cad", sleeping), text(lastTurn()).slice(-120));
    await send("stop at three");
    const steered = await until("steer bubble", () => $$(".nt-user", lastTurn()).find((u) => u.textContent.includes("stop at three")));
    await check("a message sent meanwhile joins the running turn", !!steered && steered.dataset.steer === "1");
    await check("the agent takes it", !!(await until("noted", () => text(lastTurn()).includes("Noted: stop at three"))));
    await check("no new turn for it", turns().length === before + 1, `${turns().length} vs ${before + 1}`);
    const stopPanel = $(".nt-chat.cdx"), stopTurn = lastTurn();
    if (!sameConversation(stopPanel, stopTurn)) throw sceneError("before c05-steer: no connected Codex turn to stop");
    const clicksDuringShot = [];
    const shotClick = (event) => {
      const target = event.target;
      clicksDuringShot.push({ at: performance.now(), trusted: event.isTrusted, tag: target?.tagName,
        className: String(target?.className ?? ""), text: text(target).slice(0, 160) });
    };
    document.addEventListener("click", shotClick, true);
    try {
      await say("e2e", `     before c05-steer scene: ${JSON.stringify(navigationState())}`);
      await shot("c05-steer");
      await say("e2e", `     after c05-steer scene: ${JSON.stringify(navigationState())}`);
      await say("e2e", `     clicks during c05-steer: ${JSON.stringify(clicksDuringShot)}`);
      if (!sameConversation(stopPanel, stopTurn)) {
        throw sceneError("c05-steer: the conversation disappeared or remounted while awaiting the screenshot", {
          originalPanelConnected: stopPanel?.isConnected, originalTurnConnected: stopTurn?.isConnected,
          originalTurn: stopTurn?.dataset.turn,
        });
      }
    } finally {
      document.removeEventListener("click", shotClick, true);
    }
    // "Stopping…" shows from the click until the agent confirms (only a moment with the stand-in).
    let stoppingSeen = false, conversationChanged = false;
    const watch = new MutationObserver(() => {
      if (!sameConversation(stopPanel, stopTurn)) { conversationChanged = true; return; }
      if (/Stopping…|正在停止…/.test(text($(".cdx-worked", stopTurn)))) stoppingSeen = true;
    });
    watch.observe(document.body, { subtree: true, childList: true, characterData: true });
    const stopButton = $(".nt-compose .round.stop");
    if (!stopButton?.isConnected || stopButton.disabled) {
      watch.disconnect();
      throw sceneError("Stop: no connected enabled Stop button", { expectedTurn: stopTurn.dataset.turn });
    }
    stopButton.click();
    if (/Stopping…|正在停止…/.test(text($(".cdx-worked", stopTurn)))) stoppingSeen = true;
    const stopped = await until("stopped", () => conversationChanged || $(".cdx-worked.x-stop", stopTurn));
    watch.disconnect();
    if (conversationChanged || !sameConversation(stopPanel, stopTurn)) {
      throw sceneError("Stop: the original conversation disappeared or remounted", { expectedTurn: stopTurn.dataset.turn });
    }
    await check("the status says Stopping… until the agent stops", stoppingSeen);
    await check("Stop ends the turn: 'You stopped after …'", !!stopped && /You stopped after|你在.*后停止了/.test(text(stopped)), text(stopped));
    await check("a stopped turn does not fold its work", !$(".cdx-worked button.x-tg", lastTurn()) && !!$(".cdx-work", lastTurn()));
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
    await check("eight files changed, three listed", !!many && /Edited 8 files|已编辑 8 个文件/.test(text($(".x-hd .x-ti", many))) && $$(".x-file", many).length === 3, text($(".x-hd", many)));
    const more = $(".x-files > .x-more", many);
    await check("'Show 5 more files' lists the rest", !!more && /5/.test(text(more)));
    more.click();
    await check("all eight are listed then, and they fold back", !!(await until("all files", () => $$(".x-file", many).length === 8, 3000)) && /Collapse files|收起文件/.test(text(more)));
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
    const box = bigShell && $(".x-out-box", bigShell);
    // Where the output's last line is drawn, against the box it scrolls in.
    const lastLineIn = () => {
      const node = $("pre.x-out", box).firstChild;
      const i = node.textContent.indexOf("line 300");
      const r = document.createRange();
      r.setStart(node, i);
      r.setEnd(node, i + 8);
      const a = r.getBoundingClientRect(), b = box.getBoundingClientRect();
      return a.top >= b.top - 1 && a.bottom <= b.bottom + 1;
    };
    await check("long output shows its end in a box that scrolls, lines unwrapped, nothing cut",
      !!box && box.scrollHeight > box.clientHeight + 100 && lastLineIn() && getComputedStyle($("pre.x-out", box)).whiteSpace === "pre" && !$(".x-more", bigShell),
      box && `${box.scrollHeight}/${box.clientHeight} ${getComputedStyle($("pre.x-out", box)).whiteSpace}`);
    await check("all of it is a scroll away", /line 1:/.test($("pre.x-out", box).textContent) && box.dataset.up !== undefined);
    const oldError = $$(".nt-transcript .nt-notice.error").find((node) => /overloaded/.test(text(node)));
    await check("a previous turn's failure stays in transcript flow and never overlays the next terminal",
      !!oldError && getComputedStyle(oldError).position === "static" && oldError.getBoundingClientRect().bottom <= bigShell.getBoundingClientRect().top,
      oldError && `${getComputedStyle(oldError).position}: ${oldError.getBoundingClientRect().bottom} / ${bigShell.getBoundingClientRect().top}`);
    await screenshotAt("c07-many", bigShell);

    // 9. A Codex that sends no phases: the last text of the finished turn is the answer.
    await sendTurn("NOPHASE look at the readme");
    await idle();
    await check("without phases the last message still becomes the answer, the rest folds",
      /The README describes the report tool/.test(text($(".cdx-answer", lastTurn()))) && !!$(".cdx-worked", lastTurn()), text(lastTurn()).slice(-160));

    // 10. A compaction shows as a divider in the work.
    await sendTurn("COMPACT now");
    await idle();
    await openWork(lastTurn());
    await check("a compaction shows as a quiet line", $$(".cdx-compact", lastTurn()).some((d) => /Context automatically compacted|上下文已自动压缩/.test(text(d))), text(lastTurn()).slice(-160));

    // 10b. One file edited twice, a scratch file created then deleted: the changes are the turn's net diff.
    await sendTurn("REEDIT the readme");
    await idle();
    const ch = $(".cdx-changes", lastTurn());
    await check("edits of one file count once, by the turn's net diff; a scratch file is gone",
      !!ch && !$(".x-file", ch) && /^(Edited README\.md|已编辑 README\.md)$/.test(text($(".x-hd .x-ti", ch))) && /\+4/.test(text($(".x-hd .cdx-stat", ch))) && !/−/.test(text($(".x-hd .cdx-stat", ch))),
      ch && text(ch));
    $(".x-hd .btn", ch).click();
    await check("a one-file change opens its diff with Review", !!(await until("one diff", () => $(".x-one .cdx-diff", ch), 3000)));
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

    // 10e. One round button; Esc twice stops (the first press says "Esc" on it).
    await sendTurn("SLOW once more");
    await until("live sleep", () => $$(".cdx-act.is-live", lastTurn()).some((a) => /sleep 30/.test(text(a))));
    const rounds = () => $$(".nt-compose .round");
    await check("while it runs, one round button: Stop", rounds().length === 1 && rounds()[0].classList.contains("stop"), rounds().map((r) => r.className).join(" | "));
    setValue(composer(), "a thought");
    await sleep(150);
    await check("with text typed it is the one that sends, as a steer", rounds().length === 1 && /^(Steer|插话)$/.test(rounds()[0].getAttribute("aria-label")),
      rounds().map((r) => r.getAttribute("aria-label")).join(" | "));
    setValue(composer(), "");
    await sleep(150);
    const esc = () => composer().dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    // The existing workspace title menu closes with Esc, and that Esc is no step toward stopping.
    $(".ws-tb-end .ws-ib[aria-haspopup=menu]").click();
    await until("session title menu", () => $(".ws-menu"), 3000);
    keyDown(document.activeElement, "Escape", { cancelable: true });
    await sleep(200);
    await check("Esc closes the session menu, and only that", !$(".ws-menu") && text(rounds()[0]) !== "Esc", `${!!$(".ws-menu")} ${text(rounds()[0])}`);
    // With a popover open, Esc closes it and is no step toward stopping.
    const ring = $("button.nt-ring");
    if (ring) {
      ring.click();
      await until("usage popover", () => $(".nt-usage"), 3000);
      esc();
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
      await sleep(200);
      await check("Esc with a popover open only closes it", !$(".nt-usage") && text(rounds()[0]) !== "Esc", `${!!$(".nt-usage")} ${text(rounds()[0])}`);
    }
    esc();
    await sleep(200);
    await check("Esc once: it still runs, the button says Esc", lastTurn().classList.contains("running") && text(rounds()[0]) === "Esc", text(rounds()[0]));
    await sleep(2300);
    await check("the press is forgotten after 2 s", text(rounds()[0]) !== "Esc" && lastTurn().classList.contains("running"));
    const escPair = await escapeTwiceWithinWindow($(".nt-compose"), esc, () => text(rounds()[0]) === "Esc");
    await say("e2e", `     Esc pair timing: ${JSON.stringify(escPair)}`);
    await check("Esc twice stops the turn", !!(await until("stopped by Esc", () => $(".cdx-worked.x-stop", lastTurn()), 10000)));
    await idle();

    // 10f. While the answer streams: a message waits for the turn to end; moving up lets go of the end.
    await sendTurn("DRAWL tell me about it");
    await until("answer streaming", () => $(".cdx-answer.streaming", lastTurn()), 15000);
    const nDrawl = turns().length;
    setValue(composer(), "QUEUED after the answer");
    await sleep(150);
    await check("sending while the answer streams queues it", /^(Queue|排队)$/.test(rounds()[0].getAttribute("aria-label")), rounds()[0].getAttribute("aria-label"));
    rounds()[0].click();
    await until("composer cleared", () => composer().value === "", 5000);
    const body = $(".nt-chat-body");
    const fromEnd = () => body.scrollHeight - body.scrollTop - body.clientHeight;
    body.dispatchEvent(new WheelEvent("wheel", { deltaY: -10, bubbles: true }));
    body.scrollTop = Math.max(0, body.scrollTop - 300);
    await sleep(1500);
    await check("moving up while it streams lets go of the end", fromEnd() > 100 && $(".nt-jump").dataset.shown === "true" && !!$(".cdx-answer.streaming", lastTurn()),
      `${Math.round(fromEnd())} px from the end`);
    await check("the way back is a round button, three dots waving while it works", $$(".nt-jump .x-wave i").length === 3);
    // More streamed in below; now the list gets shorter (the composer grows): what was at its bottom
    // edge stays there, the reader's place is kept.
    const shorter = await resizeObserved(body, () => setValue(composer(), "one\ntwo\nthree\nfour"));
    const shift = shorter.after.scrollTop - shorter.before.scrollTop, shrink = shorter.before.clientHeight - shorter.after.clientHeight;
    await say("e2e", `     shorter-list geometry: ${JSON.stringify(shorter)}`);
    await check("a shorter list keeps the reader's place", shrink > 0 && Math.abs(shift - shrink) <= 4 && fromEnd() > 100, `scrolled ${shift} for ${shrink} px shorter`);
    // Near the end (closer than the list is about to grow, yet not at it): taller again, the same.
    const nearEnd = await scrollToObserved(body, body.scrollHeight - body.clientHeight - 30);
    const taller = await resizeObserved(body, () => setValue(composer(), ""));
    const grow = taller.after.clientHeight - taller.before.clientHeight, back = taller.before.scrollTop - taller.after.scrollTop;
    await say("e2e", `     taller-list geometry: ${JSON.stringify({ nearEnd, ...taller })}`);
    await check("a taller list keeps it too, even close to the end", grow > 0 && Math.abs(back - grow) <= 4, `moved ${back} for ${grow} px taller`);
    $(".nt-jump").click();
    await sleep(600);
    await check("it goes back to the end and follows again", fromEnd() <= 24 && $(".nt-jump").dataset.shown === "false", `${Math.round(fromEnd())} px`);
    await idle(30000);
    const queuedTurn = await until("queued turn", () => turns().length > nDrawl && /QUEUED after the answer/.test(text($(".nt-user", lastTurn()))), 20000);
    await check("the queued message is the next turn, not a steer", !!queuedTurn && !$$(".nt-user[data-steer='1']", turns()[nDrawl - 1]).length);
    await idle();
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

    // 12. The same workspace starts Claude through its existing CLI selector.
    await leaveSession("the new-session page opens", () => $(".ws-new .ws-composer textarea"));
    if (!(await pickAgent("Rich fake"))) throw new Error("the workspace did not choose Rich fake");
    setValue($(".ws-new .ws-composer textarea"), "RICH check the export");
    const claudeStart = await until("Claude first message ready", () => $(".ws-new .ws-send:not(:disabled)"), 5000);
    if (!claudeStart) throw new Error("the first Claude message did not become ready");
    claudeStart.click();
    await until("claude conversation", () => $(".nt-chat.cl-root .cl-turn"), 30000);
    await until("claude turn", () => $(".cl-turn .cl-foot, .cl-stopped"), 30000);
    await check("a Claude Code session keeps its own view (Claude's), not Codex's", !$(".nt-chat.cdx") && !!$(".nt-chat.cl-root .cl-turn") && !$(".cdx-turn"),
      $(".nt-chat")?.className);
    await shot("c09-claude");

    // 13. Back to the Codex session: it replays the same way, loading first, never "empty".
    let emptySeen = false, loadingSeen = false;
    const lw = new MutationObserver(() => {
      if ($(".nt-chat-empty, .cl-empty")) emptySeen = true;
      if ($(".cdx-loading, .cl-loading")) loadingSeen = true;
    });
    await leaveSession("the new-session page opens for replay", () => $$(".ws-row").length >= 2);
    lw.observe(document.body, { subtree: true, childList: true });
    await openWorkspaceSession(codexSession, "the original Codex session");
    const replayed = await until("replay", () => $(".nt-chat.cdx") && turns().length === codexTurns && turns()[0].querySelector(".cdx-worked"), 30000);
    lw.disconnect();
    await check("reopening the Codex session replays every turn in the Codex view", !!replayed, `${turns().length} of ${codexTurns} turns`);
    await check("while it loads it says so, never that the conversation is empty", loadingSeen && !emptySeen, `${loadingSeen} ${emptySeen}`);
    await check("no question card comes back", !$(".cdx-reqs"));
    t1 = turns()[0];
    await until("first turn folded", () => !$(".cdx-work", t1), 3000);
    await check("the first turn is folded again, with its changes", WORKED.test(text($(".cdx-worked", t1))) && !$(".cdx-work", t1) && !!$(".cdx-changes", t1));
    const rre = turns().find((t) => /REEDIT/.test(text($(".nt-user", t))));
    await check("the replayed net diff is the same", !!rre && /\+4/.test(text($(".cdx-changes .x-hd .cdx-stat", rre))) && /README\.md/.test(text($(".cdx-changes .x-ti", rre))), rre && text($(".cdx-changes", rre)));
    const w1 = await openWork(t1);
    await openLine($$(".cdx-act.group", w1)[0]);
    const again = lines($(".cdx-list", $$(".cdx-act.group", w1)[0]));
    await check("the replayed lines read the same", again.some((l) => /^(Read|读取了) export\.py$/.test(l)) && again.some((l) => /exit 1|退出码 1/.test(l)), again.join(" | "));
    await shot("c10-replay");
  } catch (e) {
    await say("e2e", `     script error scene: ${JSON.stringify(navigationState())}`);
    await say("e2e", `     script error stack: ${JSON.stringify(String(e?.stack ?? e))}`);
    await check("no script errors", false, e?.message ?? String(e));
  }
  for (const key of ["ai-remote.chat.last", "ai-remote.chat.cwd", "ai-remote.chat.home"]) {
    try {
      const all = JSON.parse(localStorage.getItem(key) ?? "{}");
      for (const h of Object.keys(all)) if (h === CFG.host || /^e2e-/.test(h)) delete all[h];
      localStorage.setItem(key, JSON.stringify(all));
    } catch {}
  }
  await say("e2e", `${checks} checks`);
  await say("e2e", failures ? `${failures} failed` : "all passed");
  await say("done", String(failures));
})();
