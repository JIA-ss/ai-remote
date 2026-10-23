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
}
