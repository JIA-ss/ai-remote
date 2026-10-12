// Maps a host's stream clock onto this page's clock, so a frame's capture timestamp says how long
// ago it was captured. The page pings; the host answers with its stream clock; the answer with the
// shortest round trip gives the best mapping (NTP's rule), within half that round trip either way.

/**
 * How long a reply stays a candidate. Long enough that a congested minute (every reply delayed
 * behind queued video) still maps the clock with a reply from before it; short enough that the
 * clocks' drift (tens of ppm: a few ms a minute) does not matter.
 */
const KEEP_MS = 60_000;

export interface HostClock {
  /** Records a ping about to be sent; returns its number. */
  ping(): number;
  /** A host's answer to ping `n`, stamped with its stream clock (microseconds). */
  pong(n: number, hostUs: number): void;
  /** This page's time (performance.now() milliseconds) for a host stream time, once known. */
  toLocalMs(hostUs: number): number | null;
  /** Half the best round trip: how far off `toLocalMs` may be either way. */
  errorMs(): number | null;
}

export function createHostClock(now: () => number = () => performance.now()): HostClock {
  let next = 0;
  const sent = new Map<number, number>();
  let samples: Array<{ at: number; rtt: number; zero: number }> = [];
  const best = () => {
    const recent = samples.filter((s) => now() - s.at <= KEEP_MS);
    return recent.reduce<{ rtt: number; zero: number } | null>((b, s) => (!b || s.rtt < b.rtt ? s : b), null);
  };
  return {
    ping() {
      const n = next++;
      sent.set(n, now());
      // A host that never answers (an older version) must not grow the map.
      if (sent.size > 64) sent.delete(sent.keys().next().value as number);
      return n;
    },
    pong(n, hostUs) {
      const at = sent.get(n);
      if (at === undefined) return;
      sent.delete(n);
      const t = now();
      const rtt = t - at;
      // The host stamped its reply halfway through the round trip, as best we know.
      samples.push({ at: t, rtt, zero: at + rtt / 2 - hostUs / 1000 });
      samples = samples.filter((s) => t - s.at <= KEEP_MS).slice(-256);
    },
    toLocalMs(hostUs) {
      const b = best();
      return b ? b.zero + hostUs / 1000 : null;
    },
    errorMs() {
      const b = best();
      return b ? b.rtt / 2 : null;
    },
  };
}

/**
 * Pings through `send` until the returned stop is called: ten quickly (a mapping within a second
 * of the view starting), then once a second (the clocks drift; congestion inflates some replies).
 */
export function startPinging(clock: HostClock, send: (n: number) => void): () => void {
  let sent = 0;
  let timer: number | undefined;
  const tick = () => {
    send(clock.ping());
    sent++;
    timer = window.setTimeout(tick, sent < 10 ? 100 : 1000);
  };
  tick();
  return () => window.clearTimeout(timer);
}
