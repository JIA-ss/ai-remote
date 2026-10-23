// Run in a foreground browser preview on ?view=agents. Uses the seeded demo sessions and no host.
// The returned checks cover the sidebar's numeric navigation and the new page's compact chips.
// This is browser UI evidence; the native desktop harness is a separate acceptance step.
(async () => {
  const checks = [];
  const errors = [];
  const onError = (event) => { if (event.message) errors.push(event.message); };
  window.addEventListener("error", onError);
  const check = (name, ok, detail = "") => checks.push({ name, ok: !!ok, detail });
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const $ = (selector, root = document) => root?.querySelector(selector) ?? null;
  const $$ = (selector, root = document) => [...(root?.querySelectorAll(selector) ?? [])];
  const until = async (predicate, ms = 3000) => {
    const end = performance.now() + ms;
    while (performance.now() < end) {
      if (predicate()) return true;
      await sleep(25);
    }
    return !!predicate();
  };
  const key = (type, value, modifiers = {}) => window.dispatchEvent(new KeyboardEvent(type, {
    key: value, bubbles: true, cancelable: true, ...modifiers,
  }));
  const chord = { ctrlKey: true, shiftKey: true, metaKey: false, altKey: false };
  const release = () => key("keyup", "Control", { shiftKey: true });
  const rows = () => $$(".ws-list .ws-row-main").filter((el) => el.getClientRects().length);
  const hints = () => $$(".ws-jump-key");
  const hold = async (over = {}) => {
    release();
    rows()[0]?.focus({ preventScroll: true });
    key("keydown", "Shift", { ...chord, ...over });
    await sleep(260);
  };
  try {
    await document.fonts.ready;
    check("the preview is foreground and has at least nine session rows", !document.hidden && rows().length >= 9);
    release();
    rows()[0]?.focus({ preventScroll: true });
    key("keydown", "Shift", chord);
    await sleep(100);
    check("number badges wait for the 200 ms hold", hints().length === 0);
    await sleep(160);
    const numbered = rows().slice(0, 9).map((el) => el.closest("[data-row-key]").dataset.rowKey);
    check("only the first nine rendered rows are numbered in order", hints().length === 9
      && hints().every((el, i) => el.textContent === String(i + 1)
        && el.closest("[data-row-key]").dataset.rowKey === numbered[i]));
    const badge = hints()[0];
    const style = badge && getComputedStyle(badge);
    check("badges keep the specified 16 px size and 10 px type", style?.height === "16px"
      && style?.minWidth === "16px" && style?.fontSize === "10px" && style?.borderRadius === "3px");
    key("keydown", "(", { ...chord, code: "Digit9" });
    check("Ctrl+Shift+9 opens its session even when key is shifted punctuation", await until(() => $(".ws-row[data-selected]")?.dataset.rowKey === numbered[8]));
    const selected = $(".ws-row[data-selected]");
    const box = $(".ws-list").getBoundingClientRect();
    const row = selected.getBoundingClientRect();
    check("the chosen row is scrolled into view", row.top >= box.top - 1 && row.bottom <= box.bottom + 1);
    release();
    check("releasing modifiers hides the badges", await until(() => !hints().length));

    for (const [name, action] of [
      ["another key", () => key("keydown", "x", { ...chord, code: "KeyX" })],
      ["pointer movement", () => window.dispatchEvent(new PointerEvent("pointermove", { clientX: 450, clientY: 200 }))],
      ["scrolling", () => $(".ws-list").dispatchEvent(new Event("scroll"))],
      ["window blur", () => window.dispatchEvent(new Event("blur"))],
      ["visibility change", () => document.dispatchEvent(new Event("visibilitychange"))],
      ["viewport resize", () => window.dispatchEvent(new Event("resize"))],
    ]) {
      await hold();
      const shown = hints().length > 0;
      action();
      check(`${name} removes visible number badges`, shown && await until(() => !hints().length));
    }
    for (const [name, modifiers] of [["Cmd", { metaKey: true }], ["Alt", { altKey: true }], ["Ctrl without Shift", { shiftKey: false }]]) {
      await hold(modifiers);
      check(`${name} does not activate number badges`, !hints().length);
    }
    release();
    $(".ws-side .ws-nav").click();
    await until(() => $(".ws-new .ws-composer textarea"));
    $(".ws-new .ws-composer textarea").focus();
    key("keydown", "Shift", chord);
    await sleep(260);
    check("a focused composer prevents numeric navigation", !hints().length && !!$(".ws-new"));
    release();

    const bottom = $(".ws-new-bottom");
    const environment = $(".ws-env");
    const saved = bottom.getAttribute("style");
    const width = async (px) => { bottom.style.width = `${px}px`; bottom.style.maxWidth = "none"; await sleep(80); };
    const compact = () => environment.hasAttribute("data-compact");
    try {
      await width(700);
      check("roomy environment chips show full labels", !compact());
      let threshold = null;
      for (let w = 700; w >= 200; w -= 5) {
        await width(w);
        if (compact()) { threshold = w; break; }
      }
      if (threshold === null) throw new Error("the chips never compacted at a narrow width");
      await width(700);
      for (let w = threshold + 4; w >= threshold; w--) {
        await width(w);
        if (compact()) { threshold = w; break; }
      }
      check("narrow chips retain screen-reader text, names and tooltips", compact()
        && getComputedStyle($(".ws-chip-location .ws-chip-text")).position === "absolute"
        && getComputedStyle($(".ws-chip-folder .nt-folder-btn .fn")).position === "absolute"
        && $(".ws-chip-location").getAttribute("aria-label")?.length > 0
        && $(".ws-chip-location").title.length > 0 && $(".ws-chip-folder").title.length > 0
        && $(".ws-chip-folder .nt-folder-btn").textContent.trim().length > 0);
      const mode = $$(".ws-env-lead > button.ws-chip")[1];
      check("only location and folder shrink; mode and trailing labels stay visible", !!mode
        && getComputedStyle($(".ws-chip-text", mode)).position !== "absolute"
        && $$(".ws-env-trail .ws-chip-text").every((el) => getComputedStyle(el).position !== "absolute"));
      await width(threshold + 1);
      const one = compact();
      await width(threshold + 2);
      check("one or two pixels of extra width do not oscillate the labels", one && compact(), `collapse at ${threshold}px`);
      await width(threshold + 4);
      check("labels expand when there is more than 2 px spare", !compact());

      const chooseModel = async (value) => {
        $(".ws-model > button").click();
        await until(() => $(".ws-model-pop input"));
        const input = $(".ws-model-pop input");
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(input, value);
        input.dispatchEvent(new Event("input", { bubbles: true }));
        input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
        await sleep(100);
      };
      await chooseModel("a-much-longer-model-name-for-the-chip-layout");
      check("model text changes trigger compact measurement without resizing", compact());
      await chooseModel("");
      check("shorter text restores the full labels without resizing", !compact());
    } finally {
      if (saved === null) bottom.removeAttribute("style");
      else bottom.setAttribute("style", saved);
    }
  } catch (error) {
    check("the UI test completes without an exception", false, error?.stack ?? String(error));
  } finally {
    release();
  }
  await sleep(150);
  window.removeEventListener("error", onError);
  check("the interactions raise no browser errors", errors.length === 0, errors.join("; "));
  return { passed: checks.filter((x) => x.ok).length, failed: checks.filter((x) => !x.ok).length, checks };
})();
