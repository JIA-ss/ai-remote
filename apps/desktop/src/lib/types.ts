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
}

export interface AgentStatus {
  device: string;
  version: string;
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
  | { kind: "hello"; host: string; device?: string; version: string; pending?: RawPending[]; driver?: Driver }
  | { kind: "session_start" | "session_end"; ts: number; session: string; peer: string; client?: "agent" | "person" }
  | ({ kind: "driver"; ts: number; by: string; note?: string } & Driver)
  | { kind: "note"; ts: number; from: string; text: string }
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
  durationMs: number | null;
  error: string | null;
}
