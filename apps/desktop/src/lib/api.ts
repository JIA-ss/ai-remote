// The app's only door to the outside: the Tauri bridge to the controller agent, or, when the page
// runs in a plain browser (design review, screenshots), a demo backend with sample computers.

import { Channel } from "@tauri-apps/api/core";
import { commands } from "../bindings";
import type { VideoCodec } from "./codec";
import { demoBackend } from "./demo";
import { hop } from "./hops";
import type {
  AgentStatus,
  AuditLine,
  Driver,
  FeedEvent,
  Host,
  HostAccess,
  Level,
  LocalController,
  LocalHost,
  PairTicket,
  NetworkSettings,
  PathKind,
  PendingApproval,
} from "./types";

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

/** A person's live input stream to a host (its `input/1` channel), while they drive. */
export interface LiveInputHandle {
  os: "windows" | "macos" | "other";
  /** The host reports its clipboard on this stream. */
  clipboard: boolean;
  /** The host accepts this computer's clipboard text on this stream. */
  clipboardWrite: boolean;
  /** Writes one or more newline-terminated JSON lines; false when the host is too slow and they were dropped. */
  send(data: string): Promise<boolean>;
  close(): void;
}

export interface TransferResult {
  remote?: string;
  local?: string;
  bytes: number;
  seconds: number;
}

export interface Backend {
  kind: "tauri" | "demo";
  status(): Promise<AgentStatus>;
  /** Cuts off a connected agent; its MCP client sees the connection close. */
  disconnectAgent(id: number): Promise<void>;
  /** Opens the system's light and dark settings (the app follows them). */
  openAppearanceSettings(): Promise<void>;
  hosts(): Promise<Host[]>;
  /** Removes this app's firewall block rules and allows inbound UDP (asks for administrator consent). */
  fixFirewall(): Promise<void>;
  pair(ticket: string, name?: string): Promise<Paired>;
  unpair(host: string): Promise<void>;
  /** `person`: the person at this device makes the call (it runs while they have taken over). */
  callTool(host: string, tool: string, args?: Record<string, unknown>, person?: boolean): Promise<ToolResult>;
  /**
   * Opens live input to `host`. `onLine` receives every JSON line the host writes (`clip`,
   * `clip_files`, `err`, `blocked`), and an empty string when the stream ends.
   */
  openInput(host: string, request: { clipboard: boolean }, onLine: (line: string) => void): Promise<LiveInputHandle>;
  /** This computer's clipboard text (null when it holds none). */
  clipboardText(): Promise<string | null>;
  setClipboardText(text: string): Promise<void>;
  /** Files on this computer's clipboard, as paths. */
  clipboardFiles(): Promise<string[]>;
  /** Sends a file to `host` (into `~/Desktop` there unless `remoteDir` says otherwise). */
  pushFile(host: string, local: string, remoteDir: string | null, onProgress: (done: number, total: number) => void): Promise<TransferResult>;
  /** Fetches a file from `host` into this computer's Downloads folder. */
  pullFile(host: string, remote: string, onProgress: (done: number, total: number) => void): Promise<TransferResult>;
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
  /** This computer's router port mapping (UPnP), whether quiet mode locks it, whether a host runs here. */
  networkSettings(): Promise<NetworkSettings>;
  /** Turns router port mapping on or off here; the controller applies it at once, a running host at its next start. */
  setPortMapping(on: boolean): Promise<NetworkSettings>;
  /** This computer as a host: the running host, the profile directory and, with `doctor`, its checks. */
  localHost(doctor: boolean): Promise<LocalHost>;
  /** A one-time pairing ticket from the host here. Shown once; never logged. */
  localPairCode(level: Level): Promise<PairTicket>;
  /** Stops answering the pair code shown now (its dialog was closed). */
  localWithdrawCode(): Promise<void>;
  /** Pauses (every controller's calls are refused) or resumes the agent on this computer. */
  localPause(on: boolean): Promise<void>;
  /** Stops the host here; `localStart` starts the installed one again. */
  localStop(): Promise<void>;
  localStart(): Promise<void>;
  /** The host's latest audit lines, newest last. */
  localRecent(limit: number): Promise<AuditLine[]>;
  /** Removes a controller's pairing here (its safety code did not match). */
  localForget(peer: string): Promise<void>;
  /** Keeps the window out of screen capture while it shows a pairing ticket. */
  protectWindow(on: boolean): Promise<void>;
  /** Opens a System Settings pane (`screen_recording`, `accessibility`, `login_items`, `privacy`). */
  openSystemSettings(pane: string): Promise<void>;
  /** Shows the profile directory (ai-remote.toml, logs) in the file manager. */
  revealProfile(): Promise<void>;
  /** Tells the app's native parts (the tray menu) which language the page shows. */
  setUiLanguage(lang: string): Promise<void>;
  /** Whether the app opens at login, in the tray only. */
  appLoginItem(): Promise<boolean>;
  setAppLoginItem(on: boolean): Promise<void>;
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
      /** The host's answer to `LiveView.ping(n)`: its stream clock in microseconds. */
      onPong?: (n: number, hostUs: number) => void;
      /** Asks for the pointer separately (`LiveView.pointer`): where it is, and new shapes. */
      onPointer?: (at: PointerPosition) => void;
      onPointerShape?: (shape: PointerShape) => void;
      onEnded?: (reason: string) => void;
    },
  ): Promise<LiveView | null>;
}

/** The host's pointer, relative to the streamed monitor in the units of `LiveView.screen`. */
export interface PointerPosition {
  x: number;
  y: number;
  visible: boolean;
  /** A shape from `onPointerShape`; 0 when the host could not read it. */
  shape: number;
}

/** A pointer picture, in the units of `LiveView.screen`; `rgba` is width * height straight-alpha pixels. */
export interface PointerShape {
  id: number;
  width: number;
  height: number;
  hotX: number;
  hotY: number;
  rgba: Uint8ClampedArray;
}

/** A kind-5 message (header, then u32 id, u16 hot spot x and y, BGRA) as a shape, or null. */
export function parsePointerShape(bytes: Uint8Array): PointerShape | null {
  if (bytes.length < 26) return null;
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const width = view.getUint16(2);
  const height = view.getUint16(4);
  const pixels = bytes.subarray(26);
  if (!width || !height || pixels.length !== width * height * 4) return null;
  const rgba = new Uint8ClampedArray(pixels.length);
  for (let i = 0; i < pixels.length; i += 4) {
    rgba[i] = pixels[i + 2];
    rgba[i + 1] = pixels[i + 1];
    rgba[i + 2] = pixels[i];
    rgba[i + 3] = pixels[i + 3];
  }
  return { id: view.getUint32(18), width, height, hotX: view.getUint16(22), hotY: view.getUint16(24), rgba };
}

export interface LiveView {
  monitors: Array<{ index: number; width: number; height: number; primary: boolean }>;
  /** Where the streamed monitor sits in screen coordinates (what the mouse tool takes). */
  screen: { x: number; y: number; width: number; height: number } | null;
  requestKeyframe(): void;
  /** Asks the host for its stream clock; the answer comes to `onPong`. Older hosts do not answer. */
  ping(n: number): void;
  /** Changes the stream's bitrate (kbit/s, at most the request); older hosts ignore it. */
  setBitrate(kbps: number): void;
  /** The pointer comes separately (to `onPointer`) and is not in the picture. Otherwise (an older
   * host, or a platform that cannot) it is drawn into the picture as before. */
  pointer: boolean;
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
  async fixFirewall() {
    unwrap(await commands.fixFirewall());
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
  async networkSettings() {
    return unwrap(await commands.networkSettings()) as NetworkSettings;
  },
  async setPortMapping(on) {
    unwrap(await commands.setPortMapping(on));
    return unwrap(await commands.networkSettings()) as NetworkSettings;
  },
  async localHost(doctor) {
    return unwrap(await commands.localHost(doctor)) as LocalHost;
  },
  async localPairCode(level) {
    return unwrap(await commands.localHostPairCode(level)) as PairTicket;
  },
  async localWithdrawCode() {
    unwrap(await commands.localHostWithdrawCode());
  },
  async localPause(on) {
    unwrap(await commands.localHostPause(on));
  },
  async localStop() {
    unwrap(await commands.localHostStop());
  },
  async localStart() {
    unwrap(await commands.localHostStart());
  },
  async localRecent(limit) {
    const v = unwrap(await commands.localHostRecent(limit)) as { recent: AuditLine[] };
    return v.recent ?? [];
  },
  async localForget(peer) {
    unwrap(await commands.localHostForget(peer));
  },
  async protectWindow(on) {
    unwrap(await commands.protectWindow(on));
  },
  async openSystemSettings(pane) {
    unwrap(await commands.openSystemSettings(pane));
  },
  async revealProfile() {
    unwrap(await commands.revealProfile());
  },
  async setUiLanguage(lang) {
    await commands.setUiLanguage(lang);
  },
  async appLoginItem() {
    return unwrap(await commands.appLoginItem());
  },
  async setAppLoginItem(on) {
    unwrap(await commands.setAppLoginItem(on));
  },
  async tellAgent(host, text) {
    unwrap(await commands.tellAgent(host, text));
  },
  async openInput(host, request, onLine) {
    const channel = new Channel<string>();
    channel.onmessage = (line) => onLine(line);
    const [id, reply] = unwrap(await commands.openInput(host, request, channel)) as [
      number,
      { os?: string; clipboard?: boolean; clipboard_write?: boolean },
    ];
    const os = reply.os === "windows" || reply.os === "macos" ? reply.os : "other";
    return {
      os,
      clipboard: reply.clipboard === true,
      clipboardWrite: reply.clipboard_write === true,
      send: async (data) => unwrap(await commands.sendInput(id, data)),
      close: () => void commands.closeInput(id),
    };
  },
  async clipboardText() {
    return unwrap(await commands.clipboardText());
  },
  async setClipboardText(text) {
    unwrap(await commands.setClipboardText(text));
  },
  async clipboardFiles() {
    return unwrap(await commands.clipboardFiles());
  },
  async pushFile(host, local, remoteDir, onProgress) {
    const channel = new Channel<[number | null, number | null]>();
    channel.onmessage = ([done, total]) => onProgress(done ?? 0, total ?? 0);
    const v = unwrap(await commands.pushFile(host, local, remoteDir, channel)) as { remote?: string; bytes?: number; seconds?: number };
    return { remote: v.remote, bytes: Number(v.bytes ?? 0), seconds: Number(v.seconds ?? 0) };
  },
  async pullFile(host, remote, onProgress) {
    const channel = new Channel<[number | null, number | null]>();
    channel.onmessage = ([done, total]) => onProgress(done ?? 0, total ?? 0);
    const v = unwrap(await commands.pullFile(host, remote, null, channel)) as { local?: string; bytes?: number; seconds?: number };
    return { local: v.local, bytes: Number(v.bytes ?? 0), seconds: Number(v.seconds ?? 0) };
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
      else if (bytes[0] === 3) {
        // A pong: the 18-byte header's timestamp is the host's stream clock, the body {"pong":n}.
        if (bytes.length < 18) return;
        const hostUs = Number(new DataView(bytes.buffer, bytes.byteOffset).getBigUint64(6));
        try {
          const n = JSON.parse(new TextDecoder().decode(bytes.subarray(18))).pong;
          if (typeof n === "number") opts.onPong?.(n, hostUs);
        } catch {
          // Not a pong this page understands.
        }
      } else if (bytes[0] === 4) {
        try {
          const at = JSON.parse(new TextDecoder().decode(bytes.subarray(18)));
          if (typeof at.x === "number" && typeof at.y === "number") {
            opts.onPointer?.({ x: at.x, y: at.y, visible: at.visible !== false, shape: Number(at.shape) || 0 });
          }
        } catch {
          // Not a position this page understands.
        }
      } else if (bytes[0] === 5) {
        const shape = parsePointerShape(bytes);
        if (shape) opts.onPointerShape?.(shape);
      } else {
        if (bytes[0] === 1 && bytes.length >= 18) hop("page", Number(new DataView(bytes.buffer, bytes.byteOffset).getBigUint64(6)));
        onFrame(bytes.buffer);
      }
    };
    const request = {
      monitor: opts.monitor ?? null,
      max_fps: opts.maxFps ?? 30,
      max_long_edge: opts.maxLongEdge ?? 1920,
      bitrate_kbps: opts.bitrateKbps ?? 6000,
      ...(opts.codecs?.length ? { codecs: opts.codecs } : {}),
      ...(opts.onPointer ? { cursor: "client" } : {}),
    };
    const [id, reply] = unwrap(await commands.watchScreen(host, request, channel)) as [
      number,
      { monitors?: LiveView["monitors"]; screen?: LiveView["screen"]; cursor?: string },
    ];
    return {
      monitors: reply.monitors ?? [],
      screen: reply.screen ?? null,
      requestKeyframe: () => void commands.requestKeyframe(id),
      ping: (n) => void commands.pingScreen(id, n),
      setBitrate: (kbps) => void commands.setScreenBitrate(id, kbps),
      pointer: reply.cursor === "client",
      stop: () => void commands.stopScreen(id),
    };
  },
};

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export const api: Backend = inTauri ? tauriBackend : demoBackend;
