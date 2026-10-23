// Native Claude details regression. Run with the isolated host/controller harness:
// E2E_SCRIPT=tools/desktop-claude-details-e2e.js python3 tools/desktop-chat-e2e.py --secs 240
// Selection checks use DOM Range, synthetic detail=1 clicks and HTMLElement.click() (detail=0)
// inside the native webview, without claiming physical input coverage. Clipboard writes stay local.
(async () => {
  const CFG = __CONFIG__;
  const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);
  const say = (level, message) => invoke("log_from_page", { level, message });
  let failures = 0, checks = 0;
  const check = async (name, ok, detail = "") => {
    checks++;
    if (!ok) failures++;
    await say("e2e", `${ok ? "ok  " : "FAIL"}  ${name}${detail ? `  (${String(detail).slice(0, 350)})` : ""}`);
  };
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const $ = (selector, root = document) => root?.querySelector(selector) ?? null;
  const $$ = (selector, root = document) => root ? Array.from(root.querySelectorAll(selector)) : [];
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
  async function remains(fn, ms = 300) {
    const end = Date.now() + ms;
    do {
      await sleep(25);
      if (!fn()) return false;
    } while (Date.now() < end);
    return true;
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

  // Fixture contract: the protocol test compares these raw values with the stand-in's messages.
  const DETAILS = {
    ansiCommand: "python ansi_demo.py",
    ansi: "\x1b[31mRED16\x1b[0m \x1b[38;5;46mGREEN256\x1b[0m \x1b[38;2;12;34;56mTRUECOLOR\x1b[0m\n"
      + "\x1b[31;44;7mINVERSE\x1b[0m\nold\rnew\nab\bX\nBEFORE_ESCAPE\x1b\nAFTER_ESCAPE\nSAFE_OSC\x1b]0;HIDDEN_OSC\x1b",
    diffCommand: "git diff -- sample.txt",
    diff: "diff --git a/sample.txt b/sample.txt\n--- a/sample.txt\n+++ b/sample.txt\n@@ -1 +1 @@\n-BEFORE_DIFF\n+AFTER_DIFF\n+OUTSIDE_HUNK\n",
    fence: "\x1b[31mFENCE_RED\x1b[0m\n" + "WRAPPED_".repeat(32),
    question: "Pick **output** for `sample.txt`",
    plain: "**Plain** `sample.txt`",
    styled: "**Styled** `sample.txt`",
  };
  // End fixture contract.

  function setValue(el, value) {
    if (!el?.isConnected) throw new Error("the input is missing or disconnected");
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, "value").set.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  }
  const text = (el) => el?.textContent ?? "";
  const workspacePanel = () => $(".ws[data-embedded=true]") ?? $(".ws");
  const conversation = () => $(".ws-session .nt-chat.cl-root", workspacePanel());
  const turns = () => $$(".cl-turn", conversation());
  const connected = () => {
    const panel = conversation();
    return panel && !$(".nt-chat-status, .cl-loading", panel) && panel;
  };
  const successful = (turn) => connected() && turn?.isConnected && !$(".nt-compose .round.stop")
    && !!$(".cl-foot.completed", turn) && !$(".cl-fail, .cl-stopped", turn);
  const required = async (what, fn, ms = 20000) => {
    const value = await until(what, fn, ms);
    if (!value) throw new Error(`${what} was not observed`);
    return value;
  };
  async function send(message) {
    await required("connected Claude conversation", connected);
    const panel = conversation(), area = $(".nt-compose textarea", panel);
    const before = new Set(turns().map((turn) => turn.dataset.turn));
    setValue(area, message);
    const button = await required("enabled Send", () => $(".nt-compose .round.send:not(:disabled)", panel));
    if (!panel.isConnected || panel !== conversation() || area !== $(".nt-compose textarea", panel)) throw new Error("conversation changed before Send");
    button.click();
    return required("the submitted message in a new turn", () => area.value === "" && panel === conversation()
      && turns().find((turn) => !before.has(turn.dataset.turn) && text($(".cl-user .txt", turn)) === message));
  }
  async function show(name, target) {
    if (!target?.isConnected) throw new Error(`${name}: screenshot target is disconnected`);
    const feed = $(".nt-chat-body", conversation());
    feed?.dispatchEvent(new WheelEvent("wheel", { deltaY: -10, bubbles: true }));
    target.scrollIntoView({ block: "start", behavior: "auto" });
    await required(`${name} target in viewport`, () => {
      const rect = target.getBoundingClientRect(), viewport = feed?.getBoundingClientRect();
      return viewport && rect.width > 0 && rect.height > 0 && rect.bottom > viewport.top && rect.top < viewport.bottom;
    }, 5000);
    await shot(name);
  }
  const token = (root, value) => $$("span", root).find((el) => text(el) === value);
  const color = (el) => el ? getComputedStyle(el).color : null;
  const selected = (option) => option.getAttribute("aria-checked") === "true";
  function select(node) {
    const range = document.createRange();
    range.selectNodeContents(node);
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
    if (selection.isCollapsed || !selection.toString()) throw new Error("native DOM selection did not retain its text");
  }
  let restoreClipboard = null;
  try {
    const clipboard = navigator.clipboard;
    if (!clipboard) throw new Error("the webview has no Clipboard API to intercept");
    const descriptor = Object.getOwnPropertyDescriptor(clipboard, "writeText");
    const originalWriteText = clipboard.writeText;
    const copies = [];
    Object.defineProperty(clipboard, "writeText", { configurable: true, value: async (value) => { copies.push(value); } });
    restoreClipboard = () => {
      if (descriptor) Object.defineProperty(clipboard, "writeText", descriptor);
      else delete clipboard.writeText;
      const restored = Object.getOwnPropertyDescriptor(clipboard, "writeText");
      return clipboard.writeText === originalWriteText && !!restored === !!descriptor
        && Object.keys(descriptor ?? {}).every((key) => restored?.[key] === descriptor[key]);
    };
    const copied = async (button, expected, name) => {
      if (!button?.isConnected) throw new Error(`${name}: Copy is missing or disconnected`);
      const count = copies.length;
      button.click();
      await required(`${name}: intercepted copy`, () => copies.length > count, 3000);
      await check(name, copies.length === count + 1 && copies.at(-1) === expected,
        `expected ${expected.length} characters; got ${copies.at(-1)?.length}`);
    };

    const pane = await required("the complete workspace", workspacePanel, 30000);
    if ($(".ws-session", pane)) {
      if (!$(".ws-side", pane)) {
        const toggle = await required("workspace sidebar toggle", () => $(".ws-corner .ws-ib", workspacePanel()));
        toggle.click();
      }
      const newSession = await required("workspace New action", () => $(".ws-side .ws-nav", workspacePanel()));
      newSession.click();
    }
    const form = await required("the workspace new-session form", () => $(".ws-new", workspacePanel())
      && $(".ws-new .ws-composer textarea", workspacePanel()) && $(".ws-new", workspacePanel()), 30000);
    await required("the isolated workspace host", () => text($(".ws-chip-location .ws-chip-text", form)) === CFG.host);
    const agentChip = await required("the workspace agent selector", () => $(".ws-env-trail .ws-chip:not(:disabled)", form));
    agentChip.click();
    const agent = await until("Rich fake in the workspace agent menu", () => $$(".ws-menu .ws-menu-item", workspacePanel())
      .find((el) => text($(".ws-menu-label", el)).trim() === "Rich fake"));
    await check("the start form offers the isolated Rich fake", !!agent);
    if (!agent) throw new Error("Rich fake is unavailable");
    agent.click();
    await required("Rich fake is chosen", () => text($(".ws-env-trail .ws-chip .ws-chip-text", form)).trim() === "Rich fake");
    $(".ws-chip-folder .nt-folder-btn", form).click();
    const query = await required("folder search", () => $("#ag-folder-q"));
    setValue(query, CFG.work);
    await required("typed work folder", () => $(".nt-folder-list .row.open"));
    query.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    const folder = CFG.work.split(/[\\/]/).filter(Boolean).at(-1);
    const use = await required("the listed work folder", () => text($(".nt-folder-crumbs .cur")) === folder
      && $(".nt-folder-crumbs .cur")?.title === CFG.work && $(".nt-folder-ft .btn.primary:not(:disabled)"));
    use.click();
    await check("the chosen folder is the isolated work directory", !!(await until("folder chosen", () => text($(".ws-chip-folder .fn", form)) === folder
      && $(".ws-chip-folder .nt-folder-lbl", form)?.title === CFG.work && !$(".nt-folder-pop"))));
    setValue($(".ws-composer textarea", form), "CODEDETAILS");
    (await required("enabled first-message Send", () => $(".ws-new .ws-send:not(:disabled)", workspacePanel()))).click();
    const first = await required("the CODEDETAILS conversation", () => connected()
      && turns().find((turn) => text($(".cl-user .txt", turn)) === "CODEDETAILS"));
    await required("the successful CODEDETAILS turn", () => successful(first), 30000);
    await check("CODEDETAILS runs in the Claude conversation", !!first && !!$(".nt-chat.cl-root") && !$(".nt-chat.cdx"));
    for (const button of $$(".cl-group-hd[aria-expanded=false]", first)) button.click();
    await required("two successful Bash rows", () => {
      const rows = $$(".cl-tool", first);
      return rows.length === 2 && rows.every((row) => row.dataset.state === "done");
    });
    for (const button of $$(".cl-tool-hd[aria-expanded=false]", first)) button.click();
    const bash = (command) => $$(".cl-bash", first).find((el) => text($(".cl-cmd code", el)).includes(command));
    const ansiBash = await required("the ANSI Bash output", () => bash(DETAILS.ansiCommand));
    const ansi = $(".cl-out", ansiBash);
    const visibleAnsi = "RED16 GREEN256 TRUECOLOR\nINVERSE\nnew\naX\nBEFORE_ESCAPE\nAFTER_ESCAPE\nSAFE_OSC";
    await check("ANSI output rewrites CR/backspace and hides incomplete OSC and ESC", text(ansi) === visibleAnsi, JSON.stringify(text(ansi)));
    await check("ANSI 16-color red is painted", color(token(ansi, "RED16")) === "rgb(200, 40, 41)", color(token(ansi, "RED16")));
    await check("ANSI 256-color green is painted", color(token(ansi, "GREEN256")) === "rgb(0, 255, 0)", color(token(ansi, "GREEN256")));
    await check("ANSI true color is painted", color(token(ansi, "TRUECOLOR")) === "rgb(12, 34, 56)", color(token(ansi, "TRUECOLOR")));
    const inverse = token(ansi, "INVERSE");
    await check("inverse video swaps foreground and background", color(inverse) === "rgb(66, 113, 174)"
      && inverse && getComputedStyle(inverse).backgroundColor === "rgb(200, 40, 41)");
    await copied($(".cl-copy", ansiBash), `$ ${DETAILS.ansiCommand}\n\n${DETAILS.ansi}`, "Bash Copy receives the original command and ANSI bytes");
    await show("details-01-terminal", ansiBash);

    const diffBash = await required("the plain git diff Bash output", () => bash(DETAILS.diffCommand));
    const diff = $(".cl-out", diffBash), add = $(".cl-out-add", diff), del = $(".cl-out-del", diff);
    await check("git diff keeps its full original text", text(diff) === DETAILS.diff.trimEnd(), JSON.stringify(text(diff)));
    await check("the hunk colors only its actual added and removed lines", text(add) === "+AFTER_DIFF\n" && text(del) === "-BEFORE_DIFF\n"
      && $$(".cl-out-add, .cl-out-del", diff).length === 2);
    const bg = (el) => el && getComputedStyle(el).backgroundColor;
    await check("diff additions and removals have different visible backgrounds", !!add && !!del && bg(add) !== bg(del)
      && !["transparent", "rgba(0, 0, 0, 0)"].includes(bg(add)) && !["transparent", "rgba(0, 0, 0, 0)"].includes(bg(del)), `${bg(add)} / ${bg(del)}`);
    await check("a plus line outside the completed hunk is not colored", $$("span", diff).some((el) => text(el).includes("+OUTSIDE_HUNK")
      && !el.matches(".cl-out-add, .cl-out-del")));
    await copied($(".cl-copy", diffBash), `$ ${DETAILS.diffCommand}\n\n${DETAILS.diff}`, "diff Copy receives the original patch without visual markup");
    await show("details-02-diff", diffBash);

    const fence = await required("the Markdown ANSI code fence", () => $(".cl-md .nt-cb", first));
    const code = $("pre code", fence), pre = $("pre", fence);
    await required("the visible Markdown code fence", () => pre?.isConnected && pre.clientWidth > 0
      && code?.getBoundingClientRect().height > 0 && getComputedStyle(pre).visibility === "visible");
    await check("a Markdown fence renders ANSI without exposing escapes", text(code) === "FENCE_RED\n" + "WRAPPED_".repeat(32)
      && color(token(code, "FENCE_RED")) === "rgb(200, 40, 41)");
    await check("ANSI fences start wrapped and do not overflow horizontally", pre.classList.contains("wrapped")
      && getComputedStyle(pre).whiteSpace === "pre-wrap" && pre.scrollWidth <= pre.clientWidth + 1, `${pre.scrollWidth}/${pre.clientWidth}`);
    await copied($$(".rh .acts button", fence).at(-1), DETAILS.fence, "fenced-code Copy receives the original ANSI source");
    await show("details-03-markdown", fence);

    const questionTurn = await send("ASKDETAILS");
    const question = await required("the formatted question", () => $(".cl-qc"));
    const shownAt = Date.now(), options = $$(".cl-qc-opts .opt[role=radio]", question).slice(0, 2);
    if (options.length !== 2) throw new Error("the formatted question did not expose two radio options");
    await check("question and option names render inline bold and code", text($(".q strong", question)) === "output"
      && text($(".q code", question)) === "sample.txt" && text($(".nm strong", options[0])) === "Plain"
      && text($(".nm code", options[1])) === "sample.txt");
    await check("option descriptions render emphasis, strike and bold", text($(".ds em", options[0])) === "raw"
      && text($(".ds del", options[1])) === "old" && text($(".ds strong", options[1])) === "new");
    await check("Markdown inside option buttons creates no nested controls or navigable links", options.every((option) =>
      !$("a, button, input, textarea, select, [tabindex], [role=button]", $(".lb", option)))
      && !!$('.ds [title="https://example.com/notes"]', options[0]));
    await check("the question's accessible name is plain text", question.getAttribute("aria-label") === "Pick output for sample.txt");
    const selectionSupport = { standard: CSS.supports("user-select", "text"), webkit: CSS.supports("-webkit-user-select", "text") };
    const selectionStyles = [$(".cl-qc-hd .q", question), ...options.map((option) => $(".lb", option))].map((el) => {
      const style = el && getComputedStyle(el);
      return { present: !!el, className: el?.className, userSelect: style?.getPropertyValue("user-select") ?? null,
        webkitUserSelect: style?.getPropertyValue("-webkit-user-select") ?? null };
    });
    await say("e2e", `     (selection CSS: ${JSON.stringify({ support: selectionSupport, nodes: selectionStyles })})`);
    await check("question and option text allow native selection", selectionStyles.every((style) => style.present
      && (selectionSupport.standard ? style.userSelect === "text" : selectionSupport.webkit && style.webkitUserSelect === "text")));
    select($(".ds", options[0]));
    options[0].dispatchEvent(new MouseEvent("click", { detail: 1, bubbles: true, cancelable: true }));
    await check("detail=1 activation after text selection does not choose this option", await remains(() =>
      question.isConnected && !selected(options[0]) && !selected(options[1])));
    select($(".cl-qc-hd .q", question));
    options[0].dispatchEvent(new MouseEvent("click", { detail: 1, bubbles: true, cancelable: true }));
    await check("a selection outside this option does not block choosing it", !!(await until("external selection accepted", () => selected(options[0]), 3000)));
    options[1].focus();
    select($(".ds", options[1]));
    let activationDetail = null;
    options[1].addEventListener("click", (event) => { activationDetail = event.detail; }, { once: true });
    options[1].click();
    await check("zero-detail activation selects even while its text is selected", activationDetail === 0
      && !!(await until("zero-detail selection accepted", () => selected(options[1]) && !selected(options[0]), 3000)));
    window.getSelection()?.removeAllRanges();
    await shot("details-04-question");
    await sleep(Math.max(0, 450 - (Date.now() - shownAt)));
    (await required("enabled question Submit", () => $(".cl-qc-ft .cl-btn.primary:not(:disabled)", question))).click();
    await required("the successful answered question turn", () => !$(".cl-qc") && successful(questionTurn));
    const receipt = $(".cl-receipt", questionTurn);
    await check("the receipt keeps the original question and answer strings", text($(".part .q", receipt)) === DETAILS.question
      && text($(".part .a", receipt)) === DETAILS.styled);
    const payload = $(".cl-md .nt-cb pre code", questionTurn);
    let answers = null;
    try { answers = JSON.parse(text(payload)); } catch {}
    await check("the real host protocol delivers the unchanged Markdown answer", answers?.[DETAILS.question] === DETAILS.styled
      && Object.keys(answers ?? {}).length === 1, text(payload));
    await show("details-05-answer", receipt);
  } catch (error) {
    await check("no script errors", false, error?.stack ?? String(error));
  } finally {
    try { window.getSelection()?.removeAllRanges(); } catch {}
    if (restoreClipboard) {
      try { await check("the local clipboard override restores", restoreClipboard()); }
      catch (error) { await check("the local clipboard override restores", false, error); }
    }
  }
  await say("e2e", `${checks} checks`);
  await say("e2e", failures ? `${failures} failed` : "all passed");
  await say("done", String(failures));
})();
