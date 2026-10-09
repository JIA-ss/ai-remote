// The app's state: paired computers and, for each, its live activity feed. Every view reads from
// here (useSyncExternalStore), so a computer's row, its session and the approvals inbox agree.

import { useSyncExternalStore } from "react";
import { api } from "./api";
import { emptyFeed, reduceFeed, type FeedState } from "./feed";
import type { FeedEvent, Host, PendingApproval } from "./types";

export type Group = "needs" | "working" | "you" | "idle" | "offline";

export interface Computer {
  host: Host;
  feed: FeedState;
  /** `windows` or `mac` once the host said so. */
  os: "windows" | "mac" | null;
  online: boolean | null;
  /** Why the feed is not running (offline, refused). */
  problem: string | null;
  /** When it went offline or its last session ended (ms). */
  since: number | null;
}

export interface Snapshot {
  ready: boolean;
  error: string | null;
  computers: Computer[];
}

/** How long after an agent's last step a computer still counts as "agent working". */
const WORKING_MS = 3 * 60_000;

export function groupOf(c: Computer, now = Date.now()): Group {
  if (c.online === false) return "offline";
  if (c.feed.pending.length > 0) return "needs";
  if (c.feed.driver.state === "person") return "you";
  const last = c.feed.steps.at(-1);
  const open = Object.keys(c.feed.sessions).length > 0;
  if (open && last && now - last.startedMs < WORKING_MS) return "working";
  return "idle";
}

/** Every approval waiting on any computer, oldest deadline first. */
export function allPending(s: Snapshot): Array<PendingApproval & { host: string }> {
  return s.computers
    .flatMap((c) => c.feed.pending.map((p) => ({ ...p, host: c.host.name })))
    .sort((a, b) => a.expiresMs - b.expiresMs);
}

class Store {
  private snap: Snapshot = { ready: false, error: null, computers: [] };
  private listeners = new Set<() => void>();
  private stops = new Map<string, () => void>();
  private started = false;

  subscribe = (l: () => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };

  get = () => this.snap;

  private set(next: Snapshot) {
    this.snap = next;
    this.listeners.forEach((l) => l());
  }

  private update(name: string, f: (c: Computer) => Computer) {
    this.set({ ...this.snap, computers: this.snap.computers.map((c) => (c.host.name === name ? f(c) : c)) });
  }

  /** Loads the paired computers and follows each one's feed. Safe to call more than once. */
  async start() {
    if (this.started) return;
    this.started = true;
    await this.refresh();
  }

  async refresh() {
    try {
      const hosts = await api.hosts();
      const known = new Map(this.snap.computers.map((c) => [c.host.name, c]));
      const computers = hosts.map(
        (host) =>
          known.get(host.name) ?? { host, feed: emptyFeed, os: null, online: null, problem: null, since: null },
      );
      for (const [name, stop] of this.stops) {
        if (!hosts.some((h) => h.name === name)) {
          stop();
          this.stops.delete(name);
        }
      }
      this.set({ ready: true, error: null, computers });
      for (const h of hosts) if (!this.stops.has(h.name)) void this.follow(h.name);
    } catch (e) {
      this.set({ ...this.snap, ready: true, error: String(e instanceof Error ? e.message : e) });
    }
  }

  private async follow(name: string) {
    this.stops.set(name, () => {});
    try {
      const stop = await api.subscribe(name, (e: FeedEvent) => this.onEvent(name, e));
      this.stops.set(name, stop);
    } catch (e) {
      this.stops.delete(name);
      this.update(name, (c) => ({
        ...c,
        online: false,
        problem: String(e instanceof Error ? e.message : e),
        since: c.since ?? Date.now(),
      }));
      // Try again in a while: the host may come back.
      setTimeout(() => {
        if (this.snap.computers.some((c) => c.host.name === name)) void this.follow(name);
      }, 30_000);
    }
  }

  private onEvent(name: string, e: FeedEvent) {
    this.update(name, (c) => {
      const feed = reduceFeed(c.feed, e);
      if (e.kind === "hello") {
        const os = e.os === "macos" ? "mac" : e.os === "windows" ? "windows" : c.os;
        return { ...c, feed, os, online: true, problem: null, since: null };
      }
      if (e.kind === "closed") {
        this.stops.delete(name);
        setTimeout(() => void this.follow(name), 5_000);
        return { ...c, feed, online: false, since: Date.now() };
      }
      if (e.kind === "session_end") return { ...c, feed, since: e.ts };
      return { ...c, feed };
    });
  }
}

export const store = new Store();

export function useStore(): Snapshot {
  return useSyncExternalStore(store.subscribe, store.get);
}

export function useComputer(name: string | null): Computer | null {
  const s = useStore();
  return name ? (s.computers.find((c) => c.host.name === name) ?? null) : null;
}
