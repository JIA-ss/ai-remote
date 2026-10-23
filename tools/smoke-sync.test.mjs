// Exercise the helpers embedded in the native smoke scripts without starting a desktop app.
// A virtual clock reproduces background timer throttling and a delayed native screenshot ack.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const scripts = [
  "desktop-chat-e2e.js",
  "desktop-chat-e2e-render.js",
  "desktop-chat-offline-e2e.js",
  "desktop-chat-preview-e2e.js",
  "desktop-chat-codex-e2e.js",
  "desktop-workspace-e2e.js",
  "desktop-chat-e2e-real.js",
  "desktop-chat-codex-real.js",
];

function fixture(script, send) {
  const source = readFileSync(new URL(script, import.meta.url), "utf8");
  const start = source.indexOf("  async function until(");
  const end = source.indexOf("\n  function setValue(", start);
  assert.ok(start >= 0 && end > start, `${script}: smoke helper boundaries`);
  let now = 0;
  let nextTimer = 0;
  const timers = new Map();
  const logs = [];
  const window = {};
  const setTimeout = (callback, delay) => {
    const id = ++nextTimer;
    timers.set(id, { callback, at: now + delay });
    return id;
  };
  const context = {
    window,
    Date: { now: () => now },
    setTimeout,
    clearTimeout: (id) => timers.delete(id),
    sleep: (delay) => new Promise((resolve) => setTimeout(resolve, delay)),
    say: (level, message) => {
      logs.push({ level, message });
      return send?.(level, message);
    },
  };
  vm.runInNewContext(`${source.slice(start, end)}\nglobalThis.sync = { until, shot, pending: () => shotWaits.size };`, context,
    { filename: script });
  return {
    ...context.sync,
    window,
    logs,
    timers,
    async advance(delay) {
      now += delay;
      for (const [id, timer] of [...timers]) {
        if (timer.at <= now && timers.delete(id)) timer.callback();
      }
      // Let awaits in the embedded helpers resume after the simulated late timer wakes.
      await Promise.resolve();
      await Promise.resolve();
    },
  };
}

for (const script of scripts) {
  test(`${script}: a timer waking after the deadline checks the actual final state`, async () => {
    const f = fixture(script);
    let ready = null;
    const waiting = f.until("the quoted composer", () => ready, 100);
    const actual = { composer: "> Fixed the quarter export" };
    ready = actual;
    await f.advance(1000);
    assert.equal(await waiting, actual);
    assert.equal(f.logs.length, 0, "a state that is now ready must not be reported as a timeout");
  });

  test(`${script}: a failed final check still reports the timeout`, async () => {
    const f = fixture(script);
    let checks = 0;
    const waiting = f.until("a missing state", () => {
      checks++;
      throw new Error("the element is still absent");
    }, 100);
    await f.advance(1000);
    assert.equal(await waiting, null);
    assert.equal(checks, 2, "check once before sleeping and once after the late wake");
    assert.equal(f.logs.at(-1)?.message, "     (timed out waiting for a missing state)");
  });

  test(`${script}: delayed native capture keeps the page on the same state until its ack`, async () => {
    const f = fixture(script);
    let advanced = false;
    const flow = (async () => {
      await f.shot("14-subagent");
      advanced = true;
    })();
    await f.advance(5000);
    assert.equal(advanced, false, "five seconds passing is not a screenshot acknowledgement");
    await f.advance(15000);
    assert.equal(advanced, false);
    assert.equal(f.pending(), 1);
    assert.deepEqual(f.logs, [{ level: "shot", message: "14-subagent" }]);
    // Older native builds acknowledge successful capture with just the name.
    f.window.__smokeShot("14-subagent");
    await flow;
    assert.equal(advanced, true);
    assert.equal(f.pending(), 0);
    assert.equal(f.timers.size, 0);
  });

  test(`${script}: a missing native ack rejects and prevents the next UI action`, async () => {
    const f = fixture(script);
    let advanced = false;
    const flow = (async () => {
      await f.shot("15-find");
      advanced = true;
    })();
    const rejected = assert.rejects(flow, /screenshot 15-find failed: native acknowledgement not received within 60000 ms/);
    await f.advance(60000);
    await rejected;
    assert.equal(advanced, false);
    assert.equal(f.pending(), 0);
    assert.equal(f.timers.size, 0);
    assert.doesNotThrow(() => f.window.__smokeShot("15-find"), "ignore an acknowledgement arriving after failure");
  });

  test(`${script}: a native capture error fails the screenshot and clears its timeout`, async () => {
    const f = fixture(script);
    const waiting = f.shot("16-quote");
    const rejected = assert.rejects(waiting, /screenshot 16-quote failed: capture failed: no image/);
    f.window.__smokeShot("16-quote", "capture failed: no image");
    await rejected;
    assert.equal(f.pending(), 0);
    assert.equal(f.timers.size, 0);
  });

  test(`${script}: rejected native requests fail without waiting for the capture timeout`, async () => {
    const f = fixture(script, () => Promise.reject(new Error("native bridge unavailable")));
    await assert.rejects(f.shot("bridge"), /screenshot bridge failed: Error: native bridge unavailable/);
    assert.equal(f.pending(), 0);
    assert.equal(f.timers.size, 0);
  });

  test(`${script}: synchronous bridge errors fail and duplicate names keep their original waiter`, async () => {
    const broken = fixture(script, () => { throw new Error("native bridge threw"); });
    await assert.rejects(broken.shot("bridge"), /screenshot bridge failed: Error: native bridge threw/);
    assert.equal(broken.pending(), 0);
    assert.equal(broken.timers.size, 0);

    const f = fixture(script);
    const first = f.shot("same");
    await assert.rejects(f.shot("same"), /already waiting for its native acknowledgement/);
    assert.equal(f.pending(), 1);
    f.window.__smokeShot("same", null);
    await first;
    assert.equal(f.pending(), 0);
    assert.equal(f.timers.size, 0);
  });

  test(`${script}: screenshot preparation completes before the native capture request`, async () => {
    const order = [];
    const f = fixture(script, () => order.push("native request"));
    let prepared;
    f.window.__smokePrepareShot = (name) => {
      order.push(`prepare ${name}`);
      return new Promise((resolve) => { prepared = resolve; });
    };
    const waiting = f.shot("terminal-state");
    assert.deepEqual(order, ["prepare terminal-state"]);
    await f.advance(1000);
    assert.equal(f.logs.length, 0, "preparing the screenshot must finish before any capture request");
    order.push("preparation complete");
    prepared();
    await f.advance(0);
    assert.deepEqual(order, ["prepare terminal-state", "preparation complete", "native request"]);
    f.window.__smokeShot("terminal-state");
    await waiting;
    assert.equal(f.pending(), 0);
    assert.equal(f.timers.size, 0);
  });

  test(`${script}: failed screenshot preparation stops the run without requesting capture`, async () => {
    for (const asynchronous of [false, true]) {
      const f = fixture(script);
      f.window.__smokePrepareShot = () => {
        const error = new Error("finite animation could not finish");
        if (asynchronous) return Promise.reject(error);
        throw error;
      };
      let advanced = false;
      const flow = (async () => {
        await f.shot("terminal-state");
        advanced = true;
      })();
      await assert.rejects(flow, /screenshot terminal-state failed: Error: finite animation could not finish/);
      assert.equal(advanced, false);
      assert.equal(f.logs.length, 0);
      assert.equal(f.pending(), 0);
      assert.equal(f.timers.size, 0);
    }
  });
}

function preparationFixture() {
  const source = readFileSync(new URL("desktop-chat-e2e.py", import.meta.url), "utf8");
  const match = source.match(/^SMOKE_PREPARE_SHOT = r'''\n([\s\S]*?)^'''/m);
  assert.ok(match, "load the exact screenshot preparation injected by the Python harness");
  const calls = [];
  const logs = [];
  const animations = [];
  const context = {
    window: { __TAURI_INTERNALS__: { invoke(cmd, args) {
      assert.equal(cmd, "log_from_page");
      const detail = JSON.parse(args.message.slice(args.message.indexOf("{")));
      logs.push(detail);
      calls.push(`log ${detail.phase}`);
      return Promise.resolve();
    } } },
    document: {
      getAnimations: () => animations,
      documentElement: { getBoundingClientRect() { calls.push("layout"); return { width: 1280, height: 900 }; } },
      body: { get offsetHeight() { calls.push("body layout"); return 900; } },
    },
    getComputedStyle: (el) => ({ opacity: el.opacity ?? "1", transform: el.transform ?? "none" }),
  };
  vm.runInNewContext(match[1], context, { filename: "desktop-chat-e2e.py:SMOKE_PREPARE_SHOT" });
  function animation(name, endTime, terminal, fail = false) {
    const target = { tagName: "DIV", className: "cdx-reveal x-in", opacity: "0",
      classList: { contains: (name) => name === "cdx-reveal" },
      parentElement: { tagName: "DIV", className: "ancestor", opacity: ".5" } };
    const value = {
      animationName: name,
      currentTime: 0,
      playState: "running",
      effect: { target, getComputedTiming: () => ({ endTime }) },
      get finished() {
        assert.equal(terminal, true, `${name}: leave the live animation's finished promise alone`);
        return Promise.resolve();
      },
      finish() {
        assert.equal(terminal, true, `${name}: leave the live animation running`);
        calls.push(`finish ${name}`);
        if (fail) throw new Error("finish rejected");
        this.currentTime = endTime;
        this.playState = "finished";
        target.opacity = name === "cdx-out" ? "0" : "1";
      },
    };
    animations.push(value);
    return value;
  }
  return { prepare: context.window.__smokePrepareShot, calls, logs, animation };
}

test("injected screenshot preparation finishes finite entrances/exits and lays out their terminal state", async () => {
  const f = preparationFixture();
  const entrance = f.animation("cdx-in", 220, true);
  f.animation("cdx-out", 150, true);
  const spinner = f.animation("cdx-spin", Infinity, false);
  const infiniteEntrance = f.animation("cdx-in", Infinity, false);
  const streamingSweep = f.animation("cdx-sweep", 1000, false);
  await f.prepare("c02-work-open");
  assert.deepEqual(f.calls, ["log before", "finish cdx-in", "finish cdx-out", "layout", "body layout", "log after"]);
  assert.equal(entrance.currentTime, 220);
  assert.equal(entrance.effect.target.opacity, "1");
  for (const live of [spinner, infiniteEntrance, streamingSweep]) {
    assert.equal(live.currentTime, 0);
    assert.equal(live.playState, "running");
  }
  const before = f.logs[0].samples[0];
  assert.equal(before.currentTime, 0);
  assert.equal(before.ancestors[0].opacity, "0");
  assert.equal(before.ancestors[1].opacity, ".5", "record a partially transparent ancestor too");
  assert.equal(f.logs[0].samples[2].endTime, "Infinity", "preserve infinity rather than reporting it as null");
  assert.equal(f.logs[1].samples[0].ancestors[0].opacity, "1");
  assert.equal(f.logs[1].layout.bodyHeight, 900);
});

test("injected screenshot preparation surfaces finish errors instead of pretending the final layout was captured", async () => {
  const f = preparationFixture();
  f.animation("cdx-in", 220, true, true);
  await assert.rejects(f.prepare("c03-lines-open"), /finish rejected/);
  assert.deepEqual(f.calls, ["log before", "finish cdx-in"]);
  assert.equal(f.logs.length, 1, "retain the before sample when preparation fails");
});

function inputIsolation(visible) {
  const harness = readFileSync(new URL("./desktop-chat-e2e.py", import.meta.url), "utf8");
  const source = /SMOKE_INPUT_ISOLATION = r'''([\s\S]*?)'''/.exec(harness)?.[1];
  assert.ok(source);
  const listeners = new Map();
  vm.runInNewContext(source.replace("__ALLOW_NATIVE_INPUT__", String(visible)), {
    document: { addEventListener: (type, listener, options) => { assert.equal(options.capture, true); assert.equal(options.passive, false); listeners.set(type, listener); } },
    window: { __TAURI_INTERNALS__: { invoke: async () => {} } },
  });
  return listeners;
}

test("off-screen smoke isolates native activation without blocking injected actions or real layout notifications", () => {
  const listeners = inputIsolation(false);
  for (const type of ["click", "pointerdown", "keydown", "wheel"]) {
    let prevented = 0, stopped = 0;
    const event = { type, isTrusted: false, preventDefault: () => prevented++, stopImmediatePropagation: () => stopped++ };
    listeners.get(type)(event);
    assert.equal(prevented + stopped, 0, `${type}: injected input remains usable`);
    listeners.get(type)({ ...event, isTrusted: true });
    assert.equal(prevented, 1); assert.equal(stopped, 1);
  }
  for (const type of ["scroll", "resize", "selectionchange", "focus", "input", "change"])
    assert.equal(listeners.has(type), false, `${type}: browser layout and control notifications are preserved`);
});

test("explicit visible smoke retains the native keyboard and pointer path", () => {
  assert.equal(inputIsolation(true).size, 0);
});
