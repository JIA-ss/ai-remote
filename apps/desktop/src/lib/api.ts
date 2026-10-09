// The app's only door to the outside: the Tauri bridge to the controller agent, or, when the page
// runs in a plain browser (design review, screenshots), a demo backend with sample computers.

import { Channel } from "@tauri-apps/api/core";
import { commands } from "../bindings";
import { demoBackend } from "./demo";
import type { AgentStatus, FeedEvent, Host, Level, PathKind, PendingApproval } from "./types";

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
  hosts(): Promise<Host[]>;
  pair(ticket: string, name?: string): Promise<Paired>;
  unpair(host: string): Promise<void>;
  callTool(host: string, tool: string, args?: Record<string, unknown>): Promise<ToolResult>;
  /** Follows a host's activity; returns a function that stops it. */
  subscribe(host: string, onEvent: (e: FeedEvent) => void): Promise<() => void>;
  pending(host: string): Promise<PendingApproval[]>;
  /** Allows one waiting call; Windows Hello asks the person first. */
  approve(host: string, id: string): Promise<void>;
  deny(host: string, id: string, note?: string): Promise<void>;
  /** Whether the host accepted this device's approver key (`null`: no key on this device yet). */
  approverStatus(host: string): Promise<boolean | null>;
  enrollApprover(host: string): Promise<void>;
}

class BridgeError extends Error {}

function unwrap<T>(r: { status: "ok"; data: T } | { status: "error"; error: string }): T {
  if (r.status === "error") throw new BridgeError(r.error);
  return r.data;
}

const tauriBackend: Backend = {
  kind: "tauri",
  async status() {
    return unwrap(await commands.agentStatus()) as AgentStatus;
  },
  async hosts() {
    const raw = unwrap(await commands.listHosts()) as Array<Record<string, unknown>>;
    return raw.map((h) => ({
      name: String(h.name),
      device: (h.device as string) ?? null,
      levelAtPairing: (h.level_at_pairing as Level) ?? null,
      pairedAt: (h.paired_at as number) ?? null,
      path: ((h.path as string)?.toLowerCase() as PathKind) ?? null,
    }));
  },
  async pair(ticket, name) {
    const v = unwrap(await commands.pair(ticket, name ?? null)) as Record<string, unknown>;
    return { peer: String(v.peer), sas: String(v.sas), level: (v.level as Level) ?? null };
  },
  async unpair(host) {
    unwrap(await commands.unpair(host));
  },
  async callTool(host, tool, args = {}) {
    return unwrap(await commands.callTool(host, tool, args)) as ToolResult;
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
};

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export const api: Backend = inTauri ? tauriBackend : demoBackend;
