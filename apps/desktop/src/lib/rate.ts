// Adaptive bitrate for the live view, decided here from the latency the viewer sees. When the
// network carries less than the stream's bitrate, frames queue (in QUIC's windows, about 1.25 MB a
// stream, so up to a second at 10 Mbit/s) and the time from capture to display grows. The rise
// over the lowest latency seen recently is that queue. A queue delays every frame, while a busy
// machine (the host's encoder or this page held up for a moment) delays some: so the rise is read
// from the fastest fifth of each half second's frames, and only a rise that lasts a whole second
// (two evaluations in a row) counts.
//
// - A queue past `RISE_MS` that is not shrinking cuts the bitrate to what actually arrived over the
//   last second (the link's rate, while it is saturated) times `DRAIN` (a long queue: `DRAIN_DEEP`),
//   so the queue drains, at most once per `CUT_EVERY_MS`. While the queue shrinks nothing changes:
//   cutting again before it has drained only overshoots (measured 2026-10-12, one run on an
//   8 Mbit/s emulated link: a controller that cut by 0.7 every second of queue went from 12 to its
//   1.2 Mbit/s floor in 28 s and stayed there).
// - After `CALM_MS` with almost no queue the bitrate climbs by `RAISE`, back up to the request;
//   after `RAISES_BEFORE_FAST` such steps in a row by `RAISE_FAST` (a link that recovered is
//   found within seconds; overshooting it costs one short queue). For `PROBE_MS` after a cut it
//   stays under `SAFE` times the bitrate that queued, then probes above it again (the link may
//   have grown): a steady link sees a short queue that often, not every few seconds.

const EVAL_MS = 500;
const RISE_MS = 50;
/** A rise this long is a deep queue: drain it faster. */
const DEEP_RISE_MS = 250;
const CALM_RISE_MS = 15;
const DRAIN = 0.85;
const DRAIN_DEEP = 0.6;
/** The most one cut takes off; also the cut when nothing is known about what arrives. */
const CUT = 0.7;
const CUT_EVERY_MS = 1000;
/** A queue counts as shrinking when its rise fell by this much since the last evaluation. */
const SHRINK_MS = 5;
const RAISE = 1.15;
const RAISE_FAST = 1.3;
const RAISES_BEFORE_FAST = 2;
const CALM_MS = 3000;
const SAFE = 0.9;
const PROBE_MS = 20_000;
/** The baseline is the lowest latency over this long: queues are seconds, links change slowly. */
const BASE_WINDOW_MS = 60_000;
/** A stream that starts on a slow link queues before its first latency is known, so the baseline
 * is also held under the network's one-way delay (half the best ping) plus this for capture,
 * encoding, one frame's transfer and decoding. */
const PATH_SLACK_MS = 80;
/** The arrival rate is measured over this long. */
const ARRIVAL_WINDOW_MS = 1000;

export interface RateController {
  /** A displayed frame's latency from capture, in ms, at `now` (performance.now()). */
  observe(latencyMs: number, now: number): void;
  /** A frame of `bytes` arrived at `now`. */
  received(bytes: number, now: number): void;
  /** The network's one-way delay as best known (half the shortest ping round trip), in ms. */
  pathDelay(ms: number): void;
  /** The bitrate asked of the host now, kbit/s. */
  current(): number;
}

/**
 * `apply` is called with each new bitrate and why. The floor keeps a picture worth having: a tenth
 * of the request, and never under 500 kbit/s.
 */
export function createRateController(
  requestedKbps: number,
  apply: (kbps: number, why: string) => void,
): RateController {
  const floor = Math.max(500, Math.round(requestedKbps / 10));
  let target = requestedKbps;
  let window: number[] = [];
  // Per-second minima, for the baseline without keeping every sample.
  let minima: Array<{ at: number; ms: number }> = [];
  let arrivals: Array<{ at: number; bytes: number }> = [];
  let lastEval = -Infinity;
  let lastChange = -Infinity;
  let lastRise: number | null = null;
  let calmSince: number | null = null;
  let raises = 0;
  // The bitrate that last queued, and when it was cut.
  let queuedAt: { kbps: number; at: number } | null = null;
  let path: number | null = null;
  const set = (kbps: number, now: number, why: string) => {
    kbps = Math.round(Math.min(requestedKbps, Math.max(floor, kbps)));
    if (kbps === target) return;
    target = kbps;
    lastChange = now;
    apply(kbps, why);
  };
  /** kbit/s that arrived over the last `ARRIVAL_WINDOW_MS` (at least half of it), or null. */
  const arrivalKbps = (now: number) => {
    arrivals = arrivals.filter((a) => now - a.at < ARRIVAL_WINDOW_MS);
    // Frames arrive one interval apart; the oldest one's interval started before it arrived.
    const span = arrivals.length < 5 ? 0 : (now - arrivals[0].at) * arrivals.length / (arrivals.length - 1);
    if (span < ARRIVAL_WINDOW_MS / 2) return null;
    return arrivals.reduce((sum, a) => sum + a.bytes, 0) * 8 / span;
  };
  return {
    observe(latencyMs, now) {
      window.push(latencyMs);
      const second = Math.floor(now / 1000);
      const last = minima.at(-1);
      if (last && Math.floor(last.at / 1000) === second) last.ms = Math.min(last.ms, latencyMs);
      else minima.push({ at: now, ms: latencyMs });
      if (now - lastEval < EVAL_MS) return;
      lastEval = now;
      minima = minima.filter((m) => now - m.at <= BASE_WINDOW_MS);
      const base = Math.min(...minima.map((m) => m.ms), path === null ? Infinity : path + PATH_SLACK_MS);
      const sorted = window.sort((a, b) => a - b);
      const rise = sorted[Math.floor(sorted.length / 5)] - base;
      window = [];
      const shrinking = lastRise !== null && rise < lastRise - SHRINK_MS;
      const lasting = lastRise !== null && lastRise > RISE_MS;
      lastRise = rise;
      if (rise > RISE_MS) {
        calmSince = null;
        if (shrinking || now - lastChange < CUT_EVERY_MS) return;
        const arrived = arrivalKbps(now);
        // Under half the target arriving with a long queue is a link that dropped far below the
        // stream: waiting another half second to be sure would queue seconds more.
        const plunged = rise > DEEP_RISE_MS && arrived !== null && arrived < target * 0.5;
        if (!lasting && !plunged) return;
        let next: number;
        if (rise > DEEP_RISE_MS) {
          // A long queue for a whole second is the network's: what arrives is what it carries.
          next = arrived === null ? target * CUT : Math.min(target * CUT, arrived * DRAIN_DEEP);
        } else if (arrived !== null && arrived > target * 0.5) {
          // A short one counts only while about the target arrives; a still screen sends far less.
          next = Math.min(target * CUT, arrived * DRAIN);
        } else {
          return;
        }
        const why = `queue ${Math.round(rise)} ms` + (arrived === null ? "" : `, ${Math.round(arrived)} kbit/s arriving`);
        // A cut while the last one's queue still drains is not a new overshoot: keep the rate that queued.
        const recent = queuedAt !== null && now - queuedAt.at < PROBE_MS ? queuedAt.kbps : 0;
        queuedAt = { kbps: Math.max(target, recent), at: now };
        raises = 0;
        set(next, now, why);
      } else if (rise < CALM_RISE_MS) {
        calmSince ??= now;
        if (target < requestedKbps && now - calmSince >= CALM_MS && now - lastChange >= CALM_MS) {
          let next = target * (raises >= RAISES_BEFORE_FAST ? RAISE_FAST : RAISE);
          if (queuedAt && now - queuedAt.at < PROBE_MS) next = Math.min(next, queuedAt.kbps * SAFE);
          if (next > target) {
            raises++;
            set(next, now, "calm");
            calmSince = now;
          }
        }
      } else {
        calmSince = null;
      }
    },
    received(bytes, now) {
      arrivals.push({ at: now, bytes });
      if (arrivals.length > 2000) arrivals = arrivals.filter((a) => now - a.at <= ARRIVAL_WINDOW_MS);
    },
    pathDelay(ms) {
      path = ms;
    },
    current: () => target,
  };
}
