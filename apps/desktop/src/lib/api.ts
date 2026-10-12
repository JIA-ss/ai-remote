// The app's only door to the outside: the Tauri bridge to the controller agent, or, when the page
// runs in a plain browser (design review, screenshots), a demo backend with sample computers.

import { Channel } from "@tauri-apps/api/core";
import { commands } from "../bindings";
import type { VideoCodec } from "./codec";
import { demoBackend } from "./demo";
import type { AgentStatus, Driver, FeedEvent, Host, HostAccess, Level, LocalController, PathKind, PendingApproval } from "./types";

export type TurnAction = "take_over" | "hand_back" | "pause" | "resume";

export interface ToolContent {
  type: "text" | "image" | string;
  text?: string;
  data?: string;
  mimeType?: string;
}

export interface ToolResult {
  content: ToolContent[];
  isError?: boolean;
  structuredContent?: unknown;
}

export interface Paired {
  peer: string;
  /** The three-word safety code both machines must show. */
  sas: string;
  level: Level | null;
}

export interface Backend {
  kind: "tauri" | "demo";
  status(): Promise<AgentStatus>;
  /** Cuts off a connected agent; its MCP client sees the connection close. */
  disconnectAgent(id: number): Promise<void>;
  /** Opens the system's light and dark settings (the app follows them). */
  openAppearanceSettings(): Promise<void>;
  hosts(): Promise<Host[]>;
  pair(ticket: string, name?: string): Promise<Paired>;
  unpair(host: string): Promise<void>;
  /** `person`: the person at this device makes the call (it runs while they have taken over). */
  callTool(host: string, tool: string, args?: Record<string, unknown>, person?: boolean): Promise<ToolResult>;
  /** Follows a host's activity; returns a function that stops it. */
  subscribe(host: string, onEvent: (e: FeedEvent) => void): Promise<() => void>;
  pending(host: string): Promise<PendingApproval[]>;
  /** Allows one waiting call; Windows Hello asks the person first. */
  approve(host: string, id: string): Promise<void>;
  deny(host: string, id: string, note?: string): Promise<void>;
  /** Whether the host accepted this device's approver key (`null`: no key on this device yet). */
  approverStatus(host: string): Promise<boolean | null>;
  enrollApprover(host: string): Promise<void>;
  /** Take over (optionally letting the agent watch), hand back (with a note), pause or resume. */
  setTurn(host: string, action: TurnAction, opts?: { agentMayWatch?: boolean; note?: string }): Promise<Driver>;
  /** A message for the agent; it arrives with its next call result. */
  tellAgent(host: string, text: string): Promise<void>;
  /** What this device may do on `host`: level, standing rules, paired devices, tools. */
  access(host: string): Promise<HostAccess>;
  /** Lowers this device's level on `host`; raising it takes someone at that computer. */
  lowerLevel(host: string, level: Level): Promise<void>;
  /**
   * This computer as a host: the controllers paired with it and whether each is trusted with
   * everything (bypass). `null` when ai-remote host does not run here.
   */
  localControllers(): Promise<LocalController[] | null>;
  /** Turns bypass on or off for a controller paired with this computer. */
  setLocalBypass(peer: string, on: boolean): Promise<void>;
  /**
   * Starts a live view: raw frames (header + H.264) go to `onFrame`. Returns the host's monitors,
   * a way to ask for a keyframe, and a stop function. The demo backend has no video (`null`).
   */
  watchScreen(
    host: string,
    onFrame: (frame: ArrayBuffer) => void,
    opts?: {
      monitor?: number; maxFps?: number; maxLongEdge?: number; bitrateKbps?: number;
      /** What this machine decodes, best first (see codec.ts); the host defaults to H.264. */
      codecs?: VideoCodec[];
      onEnded?: (reason: string) => void;
    },
  ): Promise<LiveView | null>;
}

export interface LiveView {
  monitors: Array<{ index: number; width: number; height: number; primary: boolean }>;
  /** Where the streamed monitor sits in screen coordinates (what the mouse tool takes). */
  screen: { x: number; y: number; width: number; height: number } | null;
  requestKeyframe(): void;
  stop(): void;
}

class BridgeError extends Error {}

/** `https://host:port` → `host` (no trailing root dot), or null. */
function relayHost(url: unknown): string | null {
  if (typeof url !== "string" || !url) return null;
  const rest = url.includes("://") ? url.slice(url.indexOf("://") + 3) : url;
  return rest.split(/[/:]/)[0].replace(/\.$/, "") || null;
}

function unwrap<T>(r: { status: "ok"; data: T } | { status: "error"; error: string }): T {
  if (r.status === "error") throw new BridgeError(r.error);
  return r.data;
}

const tauriBackend: Backend = {
  kind: "tauri",
  async status() {
    return unwrap(await commands.agentStatus()) as AgentStatus;
  },
  async disconnectAgent(id) {
    unwrap(await commands.disconnectAgent(id));
  },
  async openAppearanceSettings() {
    unwrap(await commands.openAppearanceSettings());
  },
  async hosts() {
    const raw = unwrap(await commands.listHosts()) as Array<Record<string, unknown>>;
    return raw.map((h) => ({
      name: String(h.name),
      device: (h.device as string) ?? null,
      levelAtPairing: (h.level_at_pairing as Level) ?? null,
      pairedAt: (h.paired_at as number) ?? null,
      path: ((h.path as string)?.toLowerCase() as PathKind) ?? null,
      relay: relayHost(h.relay),
      relayN0: h.relay_n0 === true,
      p2pObstacle: (h.p2p_obstacle as Host["p2pObstacle"]) ?? null,
    }));
  },
  async pair(ticket, name) {
    const v = unwrap(await commands.pair(ticket, name ?? null)) as Record<string, unknown>;
    return { peer: String(v.peer), sas: String(v.sas), level: (v.level as Level) ?? null };
  },
  async unpair(host) {
    unwrap(await commands.unpair(host));
  },
  async callTool(host, tool, args = {}, person = false) {
    return unwrap(await commands.callTool(host, tool, args, person)) as ToolResult;
  },
  async subscribe(host, onEvent) {
    const channel = new Channel<unknown>();
    channel.onmessage = (e) => onEvent(e as FeedEvent);
    const id = unwrap(await commands.subscribeEvents(host, channel));
    return () => void commands.unsubscribeEvents(id);
  },
  async pending(host) {
    const [, list] = unwrap(await commands.pendingApprovals(host));
    return list.map((p) => ({ ...p, intent: p.intent ?? null, expiresMs: p.expiresMs ?? 0 }));
  },
  async approve(host, id) {
    unwrap(await commands.approve(host, id));
  },
  async deny(host, id, note) {
    unwrap(await commands.deny(host, id, note ?? null));
  },
  async approverStatus(host) {
    return unwrap(await commands.approverStatus(host));
  },
  async enrollApprover(host) {
    unwrap(await commands.enrollApprover(host));
  },
  async setTurn(host, action, opts = {}) {
    const v = unwrap(await commands.setTurn(host, action, opts.agentMayWatch ?? false, opts.note ?? null)) as {
      driver: Driver;
    };
    return v.driver;
  },
  async access(host) {
    return unwrap(await commands.hostAccess(host)) as HostAccess;
  },
  async lowerLevel(host, level) {
    unwrap(await commands.lowerLevel(host, level));
  },
  async localControllers() {
    const v = unwrap(await commands.localHostBypass()) as { devices: LocalController[] } | null;
    return v ? v.devices : null;
  },
  async setLocalBypass(peer, on) {
    unwrap(await commands.setLocalHostBypass(peer, on));
  },
  async tellAgent(host, text) {
    unwrap(await commands.tellAgent(host, text));
  },
  async watchScreen(host, onFrame, opts = {}) {
    const channel = new Channel<unknown>();
    channel.onmessage = (m) => {
      // WKWebView's message-handler fallback serializes raw IPC replies as byte arrays.
      // Normally connect-src permits the faster binary fetch path; retain the fallback too.
      const bytes = m instanceof ArrayBuffer
        ? new Uint8Array(m)
        : Array.isArray(m) && m.every((b) => Number.isInteger(b) && b >= 0 && b <= 255)
          ? Uint8Array.from(m)
          : null;
      if (!bytes) return;
      // Kind 2 is the local native-to-webview terminal marker, never a host video frame.
      if (bytes[0] === 2) opts.onEnded?.(new TextDecoder().decode(bytes.subarray(1)));
      else onFrame(bytes.buffer);
    };
    const request = {
      monitor: opts.monitor ?? null,
      max_fps: opts.maxFps ?? 30,
      max_long_edge: opts.maxLongEdge ?? 1920,
      bitrate_kbps: opts.bitrateKbps ?? 6000,
      ...(opts.codecs?.length ? { codecs: opts.codecs } : {}),
    };
    const [id, reply] = unwrap(await commands.watchScreen(host, request, channel)) as [
      number,
      { monitors?: LiveView["monitors"]; screen?: LiveView["screen"] },
    ];
    return {
      monitors: reply.monitors ?? [],
      screen: reply.screen ?? null,
      requestKeyframe: () => void commands.requestKeyframe(id),
      stop: () => void commands.stopScreen(id),
    };
  },
};

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export const api: Backend = inTauri ? tauriBackend : demoBackend;
