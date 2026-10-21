// The app's model of what the controller agent and the hosts report.

export type Level = "observe" | "control" | "admin";
export type PathKind = "direct" | "relay" | "unknown";

export interface Host {
  name: string;
  /** The host's device id (`eid:…`). */
  device: string | null;
  levelAtPairing: Level | null;
  /** Seconds since the Unix epoch. */
  pairedAt: number | null;
  /** Known only while a connection is open. */
  path: PathKind | null;
  /** The relay host carrying the traffic, when relayed and known. */
  relay?: string | null;
  /** The relay is one of n0's public relays (rate-limited, meant for development). */
  relayN0?: boolean;
  /** While relayed, what likely kept the connection from going direct. */
  p2pObstacle?: "udp_blocked" | "symmetric_nat" | "captive_portal" | "firewall" | "remote" | null;
  /** Where this device dials the host (`ip:port`): stored at pairing, refreshed while connected. */
  addresses?: string[];
}

/** A tool a host offers, with the level it needs and what the gate does with it for this device. */
export interface HostTool {
  name: string;
  description: string;
  level: Level;
  action: "allow" | "ask" | "deny";
}

/** What this device may do on a host (the host's `{"access":true}` reply). */
export interface HostAccess {
  /** This device's level there now (`null`: none). */
  level: Level | null;
  /** This device's name on the host. */
  you: string;
  /** The host's policy rules that concern this device, in order. */
  rules: Array<{ tool: string; action: "allow" | "ask" | "deny"; scope: "you" | "everyone" }>;
  /** Paired devices: all of them for an admin, otherwise only this one. */
  devices: Array<{ name: string; level: Level | null; revoked: boolean; you: boolean }>;
  builtin: HostTool[];
  mounts: Array<{ name: string; tools: HostTool[]; needs_desktop: boolean; lazy: boolean }>;
  /** The host's owner trusts this device with everything: every tool, no approvals, no deny rules. */
  bypass?: boolean;
}

/** A controller paired with this computer (as a host), and whether its owner bypasses it. */
/** This computer's network settings (Settings > Network). */
export interface NetworkSettings {
  /** `auto`: address lookup, hole punching, relays. `lan`: direct on the local network only. */
  mode: "auto" | "lan";
  /** Find devices on the local network and be found by them (mDNS); off by default. */
  lan_discovery: boolean;
  /** Ask the router to open a port (UPnP, NAT-PMP, PCP); off by default. */
  port_mapping: boolean;
  /** `quiet = true` in ai-remote.toml keeps it off; the setting cannot turn it on. */
  locked: boolean;
  /** ai-remote host runs here; it applies a change when it next starts. */
  host_running: boolean;
  /** The host running here is in LAN mode, reached at `host_addresses`. */
  host_lan: boolean;
  host_addresses: string[];
  /** The UDP port a LAN-mode host listens on unless its config names another. */
  lan_port: number;
}

export interface LocalController {
  name: string;
  device: string | null;
  level: Level | null;
  revoked: boolean;
  /** Every tool on this computer, no approvals, no deny rules, whatever its level. */
  bypass: boolean;
  since_ms: number | null;
  /** When it ends by itself; `null` until someone turns it off. */
  until_ms: number | null;
}

export interface AgentStatus {
  /** The app's actual MCP executable and arguments, pinned to this device's profile. */
  mcp?: { command: string; args: string[] };
  device: string;
  version: string;
  /** This machine's name. */
  name?: string;
  /** Agents (MCP clients) connected to this device's controller now. */
  agents?: ConnectedAgent[];
}

export interface ConnectedAgent {
  id: number;
  /** The MCP client's own name (`claude-code`), once it sent its first request. */
  client: string | null;
  since_ms: number;
  last_ms: number;
  calls: number;
}

/** Who drives a host: the agent, a person who took over, or nobody (paused). */
export type Driver =
  | { state: "agent" }
  | { state: "person"; device: string; since_ms: number; agent_may_watch: boolean }
  | { state: "paused"; by: string; since_ms: number };

export interface PendingApproval {
  id: string;
  peer: string;
  tool: string;
  intent: string | null;
  argsPreview: string;
  expiresMs: number;
}

/** One line of a host's activity feed: its audit records plus the stream's own notes. */
export type FeedEvent =
  | {
      kind: "hello";
      host: string;
      device?: string;
      version: string;
      /** `windows` or `macos` */
      os?: string;
      pending?: RawPending[];
      driver?: Driver;
      /** This device's level on the host now. */
      level?: Level | null;
      /** The host's owner trusts this device with everything (every tool, no approvals). */
      bypass?: boolean;
    }
  | {
      kind: "session_start" | "session_end";
      ts: number;
      session: string;
      peer: string;
      client?: "agent" | "person";
      /** The MCP client behind the session (`claude-code`). */
      agent?: string | null;
    }
  | ({ kind: "driver"; ts: number; by: string; note?: string } & Driver)
  | { kind: "note"; ts: number; from: string; text: string }
  | { kind: "task"; ts: number; session: string; peer: string; task: string }
  | {
      kind: "decision";
      ts: number;
      session: string;
      peer: string;
      tool: string;
      decision: string;
      intent?: string;
      args_hash: string;
      args_preview?: string;
      approval?: string;
      by?: string;
    }
  | {
      kind: "outcome";
      ts: number;
      session: string;
      peer: string;
      tool: string;
      args_hash: string;
      outcome: "ok" | "error" | "cancelled";
      duration_ms?: number;
      result_bytes?: number;
      error?: string;
    }
  | ({ kind: "approval"; ts: number; remote?: boolean } & RawPending)
  | { kind: "approval_resolved"; ts: number; id: string; verdict: string; by?: string; note?: string; waited_ms: number }
  | { kind: "approver_added" | "approver_removed"; ts: number; peer?: string }
  | { kind: "lagged"; missed: number }
  | { kind: "closed" };

export interface RawPending {
  id: string;
  session: string;
  peer: string;
  tool: string;
  intent?: string | null;
  args_hash: string;
  args_preview: string;
  expires_ms: number;
  host_prompt?: boolean;
}

/** Where a step stands. */
export type StepState = "running" | "done" | "failed" | "cancelled" | "waiting" | "refused";

/** One tool call, as a step on the agent's timeline. */
export interface Step {
  key: string;
  session: string;
  peer: string;
  /** A person's own call while they drove (shown in amber), not the agent's. */
  byPerson: boolean;
  tool: string;
  intent: string | null;
  argsPreview: string | null;
  state: StepState;
  /** The approval request this call waited on, if any. */
  approvalId: string | null;
  /** allow, approved, denied, timeout, unavailable, level, policy */
  decision: string | null;
  /** Who answered an approval: "host" or a device name. */
  answeredBy: string | null;
  note: string | null;
  startedMs: number;
  /** When an approval was answered (or timed out). */
  decidedMs: number | null;
  durationMs: number | null;
  error: string | null;
}
