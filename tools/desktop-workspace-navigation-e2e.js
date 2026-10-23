// Native workspace layout and numeric-navigation guards, using the isolated host/controller harness:
// E2E_SCRIPT=tools/desktop-workspace-navigation-e2e.js E2E_QUERY='view=devices&shotlang=zh&shottheme=dark' \
//   python3 tools/desktop-chat-e2e.py --secs 300
// Uses synthetic DOM events and a constrained new-page container, not physical input or OS resizing.
// Never overrides document.hidden. Positive numeric jumps remain a foreground-browser test.
(async () => {
  const CFG = __CONFIG__;
  const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);
  const say = (level, message) => invoke("log_from_page", { level, message });
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  // Once found, all form, menu, transcript and row queries belong to this one workspace. The
  // embedded screen's own controls must never satisfy a navigation or project-selection wait.
  let workspacePane = null, chordTarget = null;
  const $ = (selector, root = workspacePane ?? document) => root?.querySelector(selector) ?? null;
  const $$ = (selector, root = workspacePane ?? document) => [...(root?.querySelectorAll(selector) ?? [])];
  const text = (el) => el?.textContent?.trim() ?? "";
  let checks = 0, failures = 0;
  const check = async (name, ok, detail = "") => {
    checks++;
    if (!ok) failures++;
    await say("e2e", `${ok ? "ok  " : "FAIL"}  ${name}${detail ? `  (${String(detail).slice(0, 350)})` : ""}`);
  };
  async function required(name, predicate, ms = 15000) {
    const end = Date.now() + ms;
    do {
      const value = predicate();
      if (value) return value;
      await sleep(50);
    } while (Date.now() < end);
    throw new Error(`${name} was not observed`);
  }
  const shotWaits = new Map();
  window.__smokeShot = (name, error) => shotWaits.get(name)?.(error);
  const shot = (name) => new Promise((resolve, reject) => {
    if (shotWaits.has(name)) { reject(new Error(`screenshot ${name} is already pending`)); return; }
    const finish = (error) => {
      if (shotWaits.get(name) !== finish) return;
      clearTimeout(timer);
      shotWaits.delete(name);
      if (error) reject(new Error(`screenshot ${name}: ${error}`));
      else resolve();
    };
    const timer = setTimeout(() => finish("native acknowledgement timed out"), 60000);
    shotWaits.set(name, finish);
    Promise.resolve(window.__smokePrepareShot?.(name)).then(() => {
      if (shotWaits.get(name) === finish) return say("shot", name);
    }).catch((error) => finish(String(error)));
  });
  function setValue(el, value) {
    if (!el?.isConnected) throw new Error("the input is missing or disconnected");
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, "value").set.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  }
  const key = (target, type, value, modifiers = {}) => target.dispatchEvent(new KeyboardEvent(type, {
    key: value, bubbles: true, cancelable: true, ...modifiers,
  }));
  const modifiers = { ctrlKey: true, shiftKey: true, metaKey: false, altKey: false };
  const release = () => {
    if (chordTarget?.isConnected) key(chordTarget, "keyup", "Control", { shiftKey: true });
    chordTarget = null;
  };
  const current = () => $(".ws-row[data-selected]")?.dataset.rowKey ?? null;
  async function showSidebar() {
    if (!workspacePane?.isConnected) throw new Error("the selected workspace was disconnected");
    if (!$(".ws-side")) (await required("workspace sidebar toggle", () => $(".ws-corner .ws-ib"))).click();
    return required("workspace session sidebar", () => $(".ws-side"));
  }
  try {
    await say("e2e", `     (native context: ${JSON.stringify({ hidden: document.hidden, visibility: document.visibilityState })})`);
    if (!$(".ws", document)) {
      const mode = await required("the computers/agents switch", () => $('.ws-modes [data-mode="agents"]', document));
      mode.click();
    }
    workspacePane = await required("one active complete workspace", () => {
      const panes = $$(".ws", document).filter((pane) => !pane.closest("[inert]"));
      if (panes.length > 1) throw new Error("the navigation fixture found more than one active workspace");
      return panes[0];
    });
    const divider = $(".nt-live-divider", document);
    if (divider) {
      // Use the product's real split handler, leaving OS geometry and document visibility alone.
      for (let i = 0; i < 12; i++) { key(divider, "keydown", "ArrowLeft"); await sleep(20); }
    }
    await showSidebar();
    await required("the workspace sidebar is docked", () => workspacePane.dataset.side === "docked");
    if (!$(".ws-new .ws-composer textarea")) {
      $(".ws-side .ws-nav").click();
    }
    await required("the workspace new-session form", () => $(".ws-new .ws-composer textarea"));
    if (text($(".ws-chip-location .ws-chip-text")) !== CFG.host) {
      $(".ws-chip-location").click();
      const host = await required("the isolated computer in the environment menu", () => $$(".ws-menu .ws-menu-item:not(:disabled)")
        .find((el) => text($(".ws-menu-label", el)) === CFG.host));
      host.click();
      await required("the isolated host is chosen", () => text($(".ws-chip-location .ws-chip-text")) === CFG.host);
    }
    const agent = await required("the enabled agent chip", () => $(".ws-env-trail .ws-chip:not(:disabled)"));
    agent.click();
    const rich = await required("the isolated Rich fake", () => $$(".ws-menu [role=menuitemradio]")
      .find((el) => text($(".ws-menu-label", el)) === "Rich fake"));
    rich.click();
    const fixtureSelected = () => text($(".ws-chip-location .ws-chip-text")) === CFG.host
      && text($(".ws-env-trail .ws-chip .ws-chip-text")) === "Rich fake";
    await required("the chosen fixture runtime and host", fixtureSelected);
    await check("the form uses the isolated host and Rich fake", fixtureSelected());

    $(".ws-chip-folder .nt-folder-btn").click();
    const query = await required("folder search", () => $("#ag-folder-q"));
    setValue(query, CFG.work);
    await required("the typed isolated work folder", () => $(".nt-folder-list .row.open"));
    query.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    const folder = CFG.work.split(/[\\/]/).filter(Boolean).at(-1);
    const use = await required("the browsed work folder", () => text($(".nt-folder-crumbs .cur")) === folder
      && $(".nt-folder-crumbs .cur")?.title === CFG.work
      && $(".nt-folder-ft .btn.primary:not(:disabled)"));
    use.click();
    await required("the chosen work folder", () => text($(".ws-chip-folder .fn")) === folder
      && $(".ws-chip-folder .nt-folder-lbl")?.title === CFG.work && !$(".nt-folder-pop"));

    // Observe actual WebView layout after changing only this container's CSS width. No resize event
    // or ResizeObserver delivery is fabricated; the product's observers choose the compact state.
    const bottom = $(".ws-new-bottom"), environment = $(".ws-env");
    const savedStyle = bottom.getAttribute("style");
    const compact = () => environment.hasAttribute("data-compact");
    const width = async (value) => {
      bottom.style.width = `${value}px`;
      bottom.style.maxWidth = "none";
      await required("the constrained container width", () => Math.abs(bottom.getBoundingClientRect().width - value) < 1);
      await sleep(80);
    };
    const model = async (value) => {
      $(".ws-model > button").click();
      const input = await required("the model field", () => $(".ws-model-pop input"));
      setValue(input, value);
      input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
      await required("the model field closes", () => !$(".ws-model-pop"));
    };
    // The fixed 700 px layout probe must fit wholly inside the embedded workspace. Temporarily use
    // the real sidebar toggle; restore its docked state before creating or navigating a session.
    $(".ws-side .ws-corner .ws-ib").click();
    await required("the layout probe has the workspace without its sidebar", () => workspacePane.dataset.side === "hidden" && !$(".ws-side"));
    try {
      await required("room for the original 700 px layout probe", () => {
        const body = $(".ws-new-body"), style = getComputedStyle(body);
        return body.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight) >= 700;
      });
      try {
        await document.fonts.ready;
        await width(700);
        await required("full environment labels", () => !compact());
        await check("roomy chips display the location and folder labels", [$(".ws-chip-location .ws-chip-text"), $(".ws-chip-folder .fn")]
          .every((el) => el && getComputedStyle(el).position !== "absolute"));
        let threshold = null;
        for (let value = 690; value >= 200; value -= 10) {
          await width(value);
          if (compact()) { threshold = value; break; }
        }
        if (threshold === null) throw new Error("environment labels never compacted");
        await check("narrow chips keep their text and accessible names", compact()
          && [$(".ws-chip-location .ws-chip-text"), $(".ws-chip-folder .fn")]
            .every((el) => el && text(el) && getComputedStyle(el).position === "absolute")
          && !!$(".ws-chip-location").getAttribute("aria-label") && !!$(".ws-chip-location").title && !!$(".ws-chip-folder").title);
        const modeChip = $$(".ws-env-lead > button.ws-chip")[1];
        await check("only location and folder compact, leaving mode and trailing labels visible", !!modeChip
          && getComputedStyle($(".ws-chip-text", modeChip)).position !== "absolute"
          && $$(".ws-env-trail .ws-chip-text").every((el) => getComputedStyle(el).position !== "absolute"));
        await shot("n01-compact-chips");
        await width(700);
        await required("wide chips restore full labels", () => !compact());
        await check("widening restores the complete location and folder labels", [$(".ws-chip-location .ws-chip-text"), $(".ws-chip-folder .fn")]
          .every((el) => getComputedStyle(el).position !== "absolute"));
        // Use the full labels' measured width to leave a little room for a short model. Changing the
        // model through its real field must trigger MutationObserver measurement without a width change.
        const css = getComputedStyle(environment);
        const requiredWidth = $(".ws-env-lead").getBoundingClientRect().width + $(".ws-env-trail").getBoundingClientRect().width
          + parseFloat(css.columnGap) + parseFloat(css.paddingLeft) + parseFloat(css.paddingRight);
        await width(Math.ceil(requiredWidth) + 12);
        await required("short model labels fit", () => !compact());
        const fixedWidth = bottom.getBoundingClientRect().width;
        await model("a-much-longer-model-name-for-the-chip-layout");
        await required("model text triggers compact labels", compact);
        await check("model text changes compact chips without resizing the container", bottom.getBoundingClientRect().width === fixedWidth);
        await model("");
        await required("short model restores labels", () => !compact());
        await check("shorter model text restores labels without resizing", bottom.getBoundingClientRect().width === fixedWidth);
      } finally {
        if (savedStyle === null) bottom.removeAttribute("style");
        else bottom.setAttribute("style", savedStyle);
        if (text($(".ws-model .ws-chip-text")) === "a-much-longer-model-name-for-the-chip-layout") await model("");
      }
      await required("the original wide layout returns", () => !compact());
      await shot("n02-restored-chips");
    } finally {
      await showSidebar();
      await required("the original docked sidebar returns", () => workspacePane.dataset.side === "docked");
    }

    const prompt = "Navigation guard fixture";
    setValue($(".ws-new .ws-composer textarea"), prompt);
    (await required("enabled Start", () => $(".ws-new .ws-send:not(:disabled)"))).click();
    const session = await required("the created session", () => current() && $(".ws-main .cl-user .txt")?.textContent === prompt && current());
    await required("the successful fixture turn", () => $(".ws-main .cl-foot.completed") && !$(".ws-main .round.stop"));
    $(".ws-side .ws-nav").click();
    const input = await required("the new-session composer again", () => $(".ws-new .ws-composer textarea"));
    const one = await required("the real session row", () => $$(".ws-list .ws-row-main")
      .find((el) => el.closest("[data-row-key]")?.dataset.rowKey === session));
    input.focus();
    chordTarget = input;
    key(input, "keydown", "Shift", modifiers);
    key(input, "keydown", "!", { ...modifiers, code: "Digit1" });
    await sleep(300);
    await check("a native numeric chord leaves the focused new-session composer in place", input.isConnected
      && document.activeElement === input && !!$(".ws-new") && current() === null && !$(".ws-jump-key"));
    release();
    if (document.hidden) {
      one.focus({ preventScroll: true });
      await check("the hidden document gives DOM focus to a non-editing session row", document.activeElement === one);
      chordTarget = one;
      key(one, "keydown", "Shift", modifiers);
      key(one, "keydown", "!", { ...modifiers, code: "Digit1" });
      await sleep(300);
      await check("the hidden native document does not reveal badges or open another session", document.hidden
        && !!$(".ws-new") && current() === null && !$(".ws-jump-key"));
      release();
    } else {
      await say("e2e", "     (not exercised: hidden-document numeric guard; this WebView reports visible)");
    }
    await shot("n03-navigation-guards");
  } catch (error) {
    await check("no script errors", false, `${error?.message ?? String(error)}\n${error?.stack ?? ""}`);
  } finally {
    release();
  }
  await say("e2e", `${checks} checks`);
  await say("e2e", failures ? `${failures} failed` : "all passed");
  await say("done", String(failures));
})();
