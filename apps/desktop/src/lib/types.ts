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

/** The host running on this computer, as its local control channel reports it (`status`). */
export interface LocalHostStatus {
  name: string;
  device: string | null;
  version: string;
  /** Paired controllers. */
  peers: number;
  /** Controllers with a connection open right now (any channel), by name. Missing from older hosts. */
  connected?: string[];
  /** The same, with the channels each has open. Missing from older hosts. */
  controllers?: Array<{ name: string; device: string; channels: Array<"tools" | "events" | "screen" | "files"> }>;
  /** Tool calls running now, oldest first. */
  calls?: Array<{ peer: string; tool: string; person: boolean; since_ms: number }>;
  /** The last tool call that ended. */
  last_call?: { peer: string; tool: string; person: boolean; since_ms: number; ended_ms: number } | null;
  /** Missing from older hosts. */
  driver?: Driver;
  /** Why approval prompts cannot be shown now (locked desktop, ...), or `null`. */
  prompt_blocker?: string | null;
  /** The local requests this host understands; missing from hosts older than the list itself. */
  ops?: string[];
  /** Windows service mode (SYSTEM): starting it again after a stop takes an administrator. */
  system?: boolean;
  pid: number;
  /** Started at login without a console (the LaunchAgent, logon task or service). */
  background: boolean;
}

export type Grade = "skip" | "ok" | "warn" | "fail";

/** One line of the host's doctor report (session, permissions, autostart, network, ...). */
export interface DoctorCheck {
  id: string;
  grade: Grade;
  summary: string;
  fix?: string | null;
}

export interface DoctorReport {
  overall: Grade;
  /** `host` when the running host checked itself, `cli` when no host runs here. */
  checked_in: string;
  checks: DoctorCheck[];
}

/** The autostart entry of this profile's host (`ai-remote host status`). */
export interface InstallInfo {
  kind: string;
  name: string;
  installed: boolean;
  /** The service form: the macOS system LaunchAgent (login window too) or the Windows service. */
  service?: boolean;
  /** macOS: turned off under Login Items ("Allow in the Background"); it does not start until allowed. */
  disabled?: boolean;
  /** Whether it runs this app's host with this profile (`false`: another copy, e.g. a build folder). */
  current?: boolean | null;
  launches?: string | null;
}

/** What `host install` would do, shown before the system asks for an administrator. */
export interface InstallPlan {
  summary: string;
  notes: string[];
  steps: Array<{ text: string; admin: boolean }>;
  needs_elevation: boolean;
  blocker: string | null;
}

/** This computer as a host: the running host (`null` when none), the checks and the profile directory. */
export interface LocalHost {
  host: LocalHostStatus | null;
  /** A host listens but cannot be read (why); `host` is then null although one probably runs. */
  unknown?: string | null;
  /** Only when asked for (`localHost(true)`). */
  doctor: DoctorReport | null;
  /** Only when asked for (`localHost(true)`). */
  install?: InstallInfo | null;
  profile: string;
}

/** One audit line of the host here: a decision, an outcome, a session, a change of driver, ... */
export interface AuditLine {
  ts: number;
  kind: string;
  peer?: string;
  tool?: string;
  decision?: string;
  outcome?: string;
  intent?: string;
  by?: string;
  state?: string;
  device?: string;
  error?: string;
  /** `paired` lines: the safety code and the level granted. */
  sas?: string;
  level?: string;
  [key: string]: unknown;
}

/** Where the tray menu sends the page (`tray-navigate`): a settings section, and maybe an action. */
export interface TrayIntent {
  view?: string;
  sec?: string;
  /** `pair_code`: open the pairing-code dialog; `confirm_stop`: ask before stopping a service host. */
  action?: string;
  /** An element id to scroll to (`recent`). */
  anchor?: string;
  /** A failure the tray could not show itself. */
  error?: string;
  /** Distinguishes two identical intents. */
  n: number;
}

/** A one-time pairing ticket minted by the host here. */
export interface PairTicket {
  /** The whole line the other device pastes (`ai-remote pair <line>`): works with no network between them. */
  line: string;
  /** Seconds since the Unix epoch. */
  expires_at: number;
  level: Level;
  /** The short code that hands the ticket out; absent when the host could not offer one (`code_error`). */
  code?: PairCode;
  /** The QR code of `code.link`, as an SVG path over a `size` x `size` grid. */
  qr?: { size: number; path: string } | null;
  code_error?: string;
}

/** An eight-digit pair code a host shows: once, for ten minutes. */
export interface PairCode {
  code: string;
  /** `4821 0937` */
  grouped: string;
  /** `ai-remote://pair?code=...`, the QR code's content. */
  link: string;
  /** Seconds since the Unix epoch. */
  expires_at: number;
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
  | { state: "paused"; by: string; since_ms: number; /** Paused by the owner at that computer: only they can resume. */ owner?: boolean };

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
  | ({ kind: "driver"; ts: number; by: string; note?: string; /** `owner`: changed by the owner at that computer. */ origin?: string } & Driver)
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
