// Turns a host's activity feed into the agent's timeline and the approvals waiting for a person.

import type { Driver, FeedEvent, PendingApproval, RawPending, Step } from "./types";

export interface FeedState {
  host: string | null;
  steps: Step[];
  pending: PendingApproval[];
  /** Sessions open right now, by id, with the device that opened them. */
  sessions: Record<string, { peer: string; since: number; person: boolean; agent: string | null }>;
  /** The MCP client of the newest agent session seen (`claude-code`), kept after it ends. */
  agent: string | null;
  /** What the agent said it is working on (`set_task`), with when and from which device. */
  task: { text: string; ts: number; peer: string } | null;
  /** Who drives the host now. */
  driver: Driver;
  /** Changes of driver and messages to the agent, for the timeline. */
  turns: Array<{ ts: number; driver?: Driver; by: string; note?: string; message?: string }>;
  /** False once the feed ended (host stopped, connection lost). */
  live: boolean;
  missed: number;
}

export const emptyFeed: FeedState = {
  host: null,
  steps: [],
  pending: [],
  sessions: {},
  driver: { state: "agent" },
  turns: [],
  agent: null,
  task: null,
  live: true,
  missed: 0,
};

/** Steps kept in memory; older ones scroll out. */
const MAX_STEPS = 500;

function toPending(p: RawPending): PendingApproval {
  return {
    id: p.id,
    peer: p.peer,
    tool: p.tool,
    intent: p.intent ?? null,
    argsPreview: p.args_preview,
    expiresMs: p.expires_ms,
  };
}

/** A call is identified by its session, tool and argument hash until its outcome arrives. */
function callKey(session: string, tool: string, hash: string) {
  return `${session}/${tool}/${hash}`;
}

/** Index of the newest step for this call that is still open. */
function openStep(steps: Step[], key: string) {
  for (let i = steps.length - 1; i >= 0; i--) {
    const s = steps[i];
    if (s.key.startsWith(key) && (s.state === "running" || s.state === "waiting")) return i;
  }
  return -1;
}

export function reduceFeed(state: FeedState, e: FeedEvent): FeedState {
  switch (e.kind) {
    case "hello":
      return {
        ...state,
        host: e.host,
        live: true,
        pending: (e.pending ?? []).map(toPending),
        driver: e.driver ?? state.driver,
      };
    case "session_start":
      return {
        ...state,
        sessions: {
          ...state.sessions,
          [e.session]: { peer: e.peer, since: e.ts, person: e.client === "person", agent: e.agent ?? null },
        },
        agent: e.client !== "person" && e.agent ? e.agent : state.agent,
      };
    case "driver": {
      const { kind: _k, ts, by, note, ...driver } = e;
      void _k;
      const next = driver as Driver;
      return { ...state, driver: next, turns: [...state.turns, { ts, by, note, driver: next }].slice(-MAX_STEPS) };
    }
    case "task":
      return { ...state, task: { text: e.task, ts: e.ts, peer: e.peer } };
    case "note":
      return { ...state, turns: [...state.turns, { ts: e.ts, by: e.from, message: e.text }].slice(-MAX_STEPS) };
    case "session_end": {
      const sessions = { ...state.sessions };
      delete sessions[e.session];
      return { ...state, sessions };
    }
    case "approval": {
      const steps = [...state.steps];
      const key = callKey(e.session, e.tool, e.args_hash);
      steps.push({
        key: `${key}#${e.ts}`,
        session: e.session,
        peer: e.peer,
        byPerson: state.sessions[e.session]?.person ?? false,
        tool: e.tool,
        intent: e.intent ?? null,
        argsPreview: e.args_preview,
        state: "waiting",
        approvalId: e.id,
        decision: null,
        answeredBy: null,
        note: null,
        startedMs: e.ts,
        decidedMs: null,
        durationMs: null,
        error: null,
      });
      const pending = state.pending.some((p) => p.id === e.id) ? state.pending : [...state.pending, toPending(e)];
      return { ...state, steps: steps.slice(-MAX_STEPS), pending };
    }
    case "approval_resolved": {
      // The note a person sent with a denial belongs on the step it answered.
      const steps = state.steps.map((s) =>
        s.approvalId === e.id ? { ...s, answeredBy: e.by ?? s.answeredBy, note: e.note ?? null } : s,
      );
      return { ...state, steps, pending: state.pending.filter((p) => p.id !== e.id) };
    }
    case "decision": {
      if (e.tool === "set_task") return state;
      const steps = [...state.steps];
      const key = callKey(e.session, e.tool, e.args_hash);
      const refused = !["allow", "approved"].includes(e.decision);
      const i = openStep(steps, key);
      if (i >= 0 && steps[i].state === "waiting") {
        // The step already shows the approval; record how it ended.
        steps[i] = {
          ...steps[i],
          state: refused ? "refused" : "running",
          decision: e.decision,
          answeredBy: e.by ?? null,
          decidedMs: e.ts,
        };
      } else {
        steps.push({
          key: `${key}#${e.ts}`,
          session: e.session,
          peer: e.peer,
          byPerson: state.sessions[e.session]?.person ?? false,
          tool: e.tool,
          intent: e.intent ?? null,
          argsPreview: e.args_preview ?? null,
          state: refused ? "refused" : "running",
          approvalId: e.approval ?? null,
          decision: e.decision,
          answeredBy: e.by ?? null,
          note: null,
          startedMs: e.ts,
          decidedMs: null,
          durationMs: null,
          error: null,
        });
      }
      return { ...state, steps: steps.slice(-MAX_STEPS) };
    }
    case "outcome": {
      if (e.tool === "set_task") return state;
      const steps = [...state.steps];
      const i = openStep(steps, callKey(e.session, e.tool, e.args_hash));
      if (i < 0) return state;
      steps[i] = {
        ...steps[i],
        state: e.outcome === "ok" ? "done" : e.outcome === "error" ? "failed" : "cancelled",
        durationMs: e.duration_ms ?? null,
        error: e.error ?? null,
      };
      return { ...state, steps };
    }
    case "lagged":
      return { ...state, missed: state.missed + e.missed };
    case "closed":
      return { ...state, live: false };
    default:
      return state;
  }
}
