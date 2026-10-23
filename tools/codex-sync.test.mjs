// Test the native Codex E2E's event synchronization without opening a window.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

function fixture() {
  const source = readFileSync(new URL("desktop-chat-codex-e2e.js", import.meta.url), "utf8");
  const start = source.indexOf("  async function until(");
  const end = source.indexOf("\n  function setValue(", start);
  assert.ok(start >= 0 && end > start);
  let now = 0;
  let nextTimer = 0;
  const timers = new Map();
  const observers = [];
  const resizers = [];
  const setTimeout = (callback, delay) => {
    const id = ++nextTimer;
    timers.set(id, { callback, at: now + delay });
    return id;
  };
  const context = {
    window: {},
    Date: { now: () => now },
    performance: { now: () => now },
    setTimeout,
    clearTimeout: (id) => timers.delete(id),
    sleep: (delay) => new Promise((resolve) => setTimeout(resolve, delay)),
    say: () => {},
    MutationObserver: class {
      constructor(callback) { this.callback = callback; observers.push(this); }
      observe(root) { this.root = root; this.active = true; }
      disconnect() { this.active = false; }
    },
    ResizeObserver: class {
      constructor(callback) { this.callback = callback; resizers.push(this); }
      observe(root) { this.root = root; this.active = true; }
      disconnect() { this.active = false; }
    },
  };
  vm.runInNewContext(`${source.slice(start, end)}\nglobalThis.sync = { escapeTwiceWithinWindow, scrollToObserved, resizeObserved };`, context,
    { filename: "desktop-chat-codex-e2e.js" });
  return {
    ...context.sync,
    timers,
    observers,
    resizers,
    setNow(value) { now = value; },
    mutate() { for (const observer of observers) if (observer.active) observer.callback([]); },
    resize(entries) { for (const observer of resizers) if (observer.active) observer.callback(entries); },
    async advance(delay) {
      now += delay;
      for (const [id, timer] of [...timers]) if (timer.at <= now && timers.delete(id)) timer.callback();
      await Promise.resolve();
      await Promise.resolve();
    },
  };
}

function scroller() {
  let top = 800;
  const listeners = new Set();
  const actions = [];
  const el = {
    clientHeight: 400,
    scrollHeight: 1600,
    get scrollTop() { return top; },
    set scrollTop(value) { actions.push("assign scrollTop"); top = value; },
    addEventListener(type, callback) {
      assert.equal(type, "scroll");
      actions.push("subscribe scroll");
      listeners.add(callback);
    },
    removeEventListener(type, callback) {
      assert.equal(type, "scroll");
      listeners.delete(callback);
    },
    emit({ isTrusted = true, target = el } = {}) {
      for (const callback of [...listeners]) callback({ isTrusted, target });
    },
  };
  return { el, actions, listeners };
}

test("the second Esc follows the committed armed DOM immediately and records the real interval", async () => {
  const f = fixture();
  let armed = false;
  const presses = [];
  const waiting = f.escapeTwiceWithinWindow({}, () => presses.push(armed), () => armed);
  assert.deepEqual(presses, [false]);
  f.setNow(75);
  armed = true;
  f.mutate();
  const timing = await waiting;
  assert.deepEqual(presses, [false, true]);
  assert.equal(timing.elapsedMs, 75);
  assert.equal(timing.armedBeforeSecond, true);
  assert.equal(f.timers.size, 0);
  assert.equal(f.observers[0].active, false);
  f.observers[0].callback([]);
  assert.equal(presses.length, 2, "a queued observer callback must not send a third Esc");
});

test("a synchronous armed commit also sends exactly two presses without sleeping", async () => {
  const f = fixture();
  let armed = false;
  let presses = 0;
  const timing = await f.escapeTwiceWithinWindow({}, () => { presses++; armed = presses === 1; }, () => armed);
  assert.equal(presses, 2);
  assert.equal(timing.elapsedMs, 0);
  assert.equal(f.timers.size, 0);
});

test("a late DOM commit cannot stretch the two-second Esc standard when timers are throttled", async () => {
  const f = fixture();
  let armed = false;
  let presses = 0;
  const waiting = f.escapeTwiceWithinWindow({}, () => presses++, () => armed);
  const rejected = assert.rejects(waiting, /Esc armed too late for the 2 s pair:.*"elapsedMs":2001/);
  f.setNow(2001); // Observer delivered before the overdue timer, as can happen in a background page.
  armed = true;
  f.mutate();
  await rejected;
  assert.equal(presses, 1);
  assert.equal(f.timers.size, 0);
  assert.equal(f.observers[0].active, false);
});

test("missing armed DOM rejects, and an already armed previous press cannot pass as a new pair", async () => {
  const f = fixture();
  let presses = 0;
  const waiting = f.escapeTwiceWithinWindow({}, () => presses++, () => false);
  const rejected = assert.rejects(waiting, /Esc did not arm within 2000 ms:.*"armed":false/);
  await f.advance(2000);
  await rejected;
  assert.equal(presses, 1);
  assert.equal(f.timers.size, 0);
  assert.equal(f.observers[0].active, false);
  await assert.rejects(f.escapeTwiceWithinWindow({}, () => presses++, () => true), /previous press was still armed/);
  assert.equal(presses, 1);
});

test("scroll observation subscribes before assignment and only a trusted event at the target releases it", async () => {
  const f = fixture();
  const { el, actions, listeners } = scroller();
  let advanced = false;
  const waiting = f.scrollToObserved(el, 1170).then((value) => { advanced = true; return value; });
  assert.deepEqual(actions, ["subscribe scroll", "assign scrollTop"]);
  el.emit({ isTrusted: false });
  el.emit({ target: {} });
  await Promise.resolve();
  assert.equal(advanced, false, "assigning scrollTop or dispatching a synthetic scroll is insufficient");
  el.scrollTop = 1000;
  el.emit();
  await Promise.resolve();
  assert.equal(advanced, false, "a real scroll somewhere else is not the requested reading position");
  el.scrollTop = 1170;
  f.setNow(450);
  el.emit();
  const result = await waiting;
  assert.equal(result.before.scrollTop, 800);
  assert.equal(result.observed.scrollTop, 1170);
  assert.equal(result.observed.clientHeight, 400);
  assert.equal(result.observed.scrollHeight, 1600);
  assert.equal(result.elapsedMs, 450);
  assert.equal(result.events, 2);
  assert.equal(listeners.size, 0);
  assert.equal(f.timers.size, 0);
});

test("missing browser scroll rejects before resizing even though scrollTop already equals its target", async () => {
  const f = fixture();
  const { el, listeners } = scroller();
  let resized = false;
  const flow = (async () => {
    await f.scrollToObserved(el, 1170, 5000);
    resized = true;
  })();
  const rejected = assert.rejects(flow, /browser did not deliver the target scroll within 5000 ms:.*"events":0/);
  await f.advance(5000);
  await rejected;
  assert.equal(resized, false);
  assert.equal(listeners.size, 0);
  assert.equal(f.timers.size, 0);
  el.emit();
  await Promise.resolve();
  assert.equal(resized, false, "a late event cannot resume the failed test");
});

test("resize observation ignores initial and unrelated notifications and reads after the delivery round", async () => {
  const f = fixture();
  const { el } = scroller();
  let advanced = false;
  const waiting = f.resizeObserved(el, () => {
    assert.equal(f.resizers[0].active, true, "subscribe before changing the composer");
    f.resize([{ target: el }]); // The initial callback still sees the old body height.
    el.clientHeight = 342;
  }).then((value) => { advanced = true; return value; });
  f.resize([{ target: {} }]);
  await f.advance(0);
  assert.equal(advanced, false, "a new DOM height alone does not replace its browser notification");
  f.resize([{ target: el }]);
  await Promise.resolve();
  assert.equal(advanced, false, "do not resume in the middle of ResizeObserver delivery");
  // Simulate a product observer later in the same delivery round keeping the reader in place.
  el.scrollTop += 58;
  await f.advance(0);
  const result = await waiting;
  assert.equal(result.before.clientHeight, 400);
  assert.equal(result.notified.clientHeight, 342);
  assert.equal(result.notified.scrollTop, 800);
  assert.equal(result.after.scrollTop, 858);
  assert.equal(result.notifications, 2);
  assert.equal(f.resizers[0].active, false);
  assert.equal(f.timers.size, 0);
});

test("a real resize notification releases the helper even when the product keeps the wrong scroll position", async () => {
  const f = fixture();
  const { el } = scroller();
  const waiting = f.resizeObserved(el, () => { el.clientHeight = 342; });
  f.resize([{ target: el }]);
  await f.advance(0);
  const result = await waiting;
  const shrink = result.before.clientHeight - result.after.clientHeight;
  const shift = result.after.scrollTop - result.before.scrollTop;
  assert.equal(shrink, 58);
  assert.equal(shift, 0);
  assert.ok(Math.abs(shift - shrink) > 4, "the unchanged product assertion must still catch this bug");
  assert.equal(f.timers.size, 0);
});

test("a changed height without a browser resize notification rejects instead of guessing completion", async () => {
  const f = fixture();
  const { el } = scroller();
  let advanced = false;
  const flow = (async () => {
    await f.resizeObserved(el, () => { el.clientHeight = 342; }, 5000);
    advanced = true;
  })();
  const rejected = assert.rejects(flow, /browser did not deliver a changed body height within 5000 ms:.*"notifications":0/);
  await f.advance(5000);
  await rejected;
  assert.equal(advanced, false);
  assert.equal(f.resizers[0].active, false);
  assert.equal(f.timers.size, 0);
  f.resize([{ target: el }]);
  await f.advance(0);
  assert.equal(advanced, false);
});

function senderFixture(waitSteps = {}) {
  const source = readFileSync(new URL("desktop-chat-codex-e2e.js", import.meta.url), "utf8");
  const start = source.indexOf("  async function send(t)");
  const end = source.indexOf("  const idle =", start);
  assert.ok(start >= 0 && end > start);
  const state = { panel: { isConnected: true }, area: { isConnected: true, value: "" }, clicks: 0,
    button: { isConnected: true, disabled: true }, turns: [{ isConnected: true, userText: "previous message" }], observations: [] };
  state.button.click = () => {
    assert.equal(state.button.disabled, false, "never click disabled Send");
    state.clicks++;
    state.area.value = "";
  };
  const context = {
    $: (selector, root) => {
      if (selector === ".nt-chat.cdx") return state.panel;
      if (selector === ".nt-compose .round.send:not(:disabled)") return state.button.disabled ? null : state.button;
      if (selector === ".nt-user") return { textContent: root.userText };
      return null;
    },
    composer: () => state.area,
    turns: () => state.turns,
    lastTurn: () => state.turns.at(-1),
    text: (node) => node?.textContent ?? "",
    setValue: (node, value) => { node.value = value; },
    sceneError: (reason, detail) => new Error(`${reason}; ${JSON.stringify(detail)}`),
    until: async (what, condition) => {
      let result = condition();
      if (result) return result;
      for (const step of waitSteps[what] ?? []) {
        step(state);
        result = condition();
        state.observations.push({ what, ready: !!result });
        if (result) return result;
      }
      return null;
    },
  };
  vm.runInNewContext(`${source.slice(start, end)}\nglobalThis.sender = { send, sendTurn };`, context,
    { filename: "desktop-chat-codex-e2e.js:send" });
  return { state, ...context.sender };
}

test("sending waits for connected enabled Send and clicks it once after the draft renders", async () => {
  const f = senderFixture({ "message ready to send": [(state) => {
    assert.equal(state.clicks, 0);
    assert.equal(state.area.value, "SLOW count");
    state.button.disabled = false;
  }] });
  await f.send("SLOW count");
  assert.equal(f.state.clicks, 1);
  assert.equal(f.state.area.value, "");
});

test("a Send button that never enables fails explicitly instead of silently losing the message", async () => {
  const f = senderFixture();
  await assert.rejects(f.send("SLOW count"), /send: Send never became connected and enabled/);
  assert.equal(f.state.clicks, 0);
});

test("a remounted conversation before Send is a failure, never a reason to click a replacement panel", async () => {
  const f = senderFixture({ "message ready to send": [(state) => {
    state.button.disabled = false;
    state.panel = { isConnected: true };
  }] });
  await assert.rejects(f.send("SLOW count"), /send: conversation remounted before Send/);
  assert.equal(f.state.clicks, 0);
});

test("sendTurn requires a new connected turn containing the submitted message", async () => {
  const f = senderFixture({
    "message ready to send": [(state) => { state.button.disabled = false; }],
    "the new turn": [
      (state) => state.turns.push({ isConnected: true, userText: "an unrelated queued message" }),
      (state) => state.turns.push({ isConnected: true, userText: "SLOW count" }),
    ],
  });
  const turn = await f.sendTurn("SLOW count");
  assert.equal(turn.userText, "SLOW count");
  assert.deepEqual(f.state.observations.filter((entry) => entry.what === "the new turn").map((entry) => entry.ready), [false, true]);
});

test("Stop's identity check rejects a disconnected turn or a replacement conversation", () => {
  const source = readFileSync(new URL("desktop-chat-codex-e2e.js", import.meta.url), "utf8");
  const start = source.indexOf("  const sameConversation =");
  const end = source.indexOf(";\n", start) + 1;
  assert.ok(start >= 0 && end > start);
  const panel = { isConnected: true }, turn = { isConnected: true };
  const state = { panel, turn };
  const context = { $: () => state.panel, lastTurn: () => state.turn };
  vm.runInNewContext(`${source.slice(start, end)}\nglobalThis.same = sameConversation;`, context);
  assert.equal(context.same(panel, turn), true);
  turn.isConnected = false;
  assert.equal(context.same(panel, turn), false);
  turn.isConnected = true;
  state.panel = { isConnected: true };
  assert.equal(context.same(panel, turn), false);
  state.panel = panel;
  state.turn = { isConnected: true };
  assert.equal(context.same(panel, turn), false);
});
