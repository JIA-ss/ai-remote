// Measurement only (`localStorage["ai-remote.hops"] = "1"`): the wall-clock time each video frame
// reaches a stage in this page, keyed by the frame's capture timestamp, written to the app's log
// once a second ("Live video: hops [...]"). tools/hops-report.py joins them with the agent's and
// the native side's lines (AI_REMOTE_TRACE_HOPS) to show where time goes between the processes.

import { commands } from "../bindings";

export type HopStage = "page" | "decoded" | "drawn";

const enabled = (() => {
  try {
    return typeof localStorage !== "undefined" && localStorage.getItem("ai-remote.hops") === "1";
  } catch {
    return false;
  }
})();

let pending: Array<[HopStage, number, number]> = [];
// Animation frames this page gets per second, and the time spent drawing video frames.
let rafs = 0;
let drawMs = 0;
let draws = 0;

if (enabled && typeof window !== "undefined") {
  const tick = () => {
    rafs++;
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  window.setInterval(() => {
    void commands.logFromPage(
      "info",
      `Live video: page ${JSON.stringify({ raf: rafs, draws, drawMs: draws ? +(drawMs / draws).toFixed(2) : null, hidden: document.hidden })}`,
    );
    rafs = draws = drawMs = 0;
  }, 1000);
  window.setInterval(() => {
    if (!pending.length) return;
    const batch = pending;
    pending = [];
    // The app's log keeps 2000 characters of a message: 40 entries fit.
    for (let i = 0; i < batch.length; i += 40) {
      void commands.logFromPage("info", `Live video: hops ${JSON.stringify(batch.slice(i, i + 40))}`);
    }
  }, 1000);
}

/** Adds the time one `drawImage` of a video frame took. */
export function drawTime(ms: number): void {
  if (!enabled) return;
  drawMs += ms;
  draws++;
}

/** Records that the frame captured at `tsUs` (stream clock) reached `stage` now (wall clock, µs). */
export function hop(stage: HopStage, tsUs: number): void {
  if (!enabled) return;
  pending.push([stage, tsUs, Math.round((performance.timeOrigin + performance.now()) * 1000)]);
}
