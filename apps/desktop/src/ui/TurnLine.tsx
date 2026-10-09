// The turn line, the design's signature: who had the controls along time. Segments for the agent,
// you, a decision being waited on (dashed) or one that was waited on; ticks for steps and decisions;
// a head for who has the move now.

import type { FeedState } from "../lib/feed";
import { PA, PY, SeatMark } from "./glyphs";

type SegKind = "agent" | "you" | "wait" | "waited" | "idle" | "break";
type TickKind = "" | "you" | "dec" | "ring";
export type Head = "agent" | "your-move" | "you" | "paused" | "end" | "none";

export interface TurnSpec {
  segs: Array<[SegKind, number, number]>;
  ticks?: Array<[TickKind, number]>;
  head: Head;
  /** Where the head sits, in percent (default 100). */
  at?: number;
}

export function TurnLine({ spec, w, mini = false }: { spec: TurnSpec; w: number; mini?: boolean }) {
  if (w <= 0) return null;
  const h = mini ? 10 : 24;
  const y = h / 2;
  const sw = mini ? 2 : 3;
  const tH = mini ? 6 : 8;
  const tW = mini ? 1 : 1.5;
  const headH = mini ? 10 : 18;
  const room = spec.head === "none" || spec.head === "end" ? 2 : headH * 0.62;
  const L = w - room;
  const X = (p: number) => (p / 100) * L;
  const C = { agent: "var(--agent)", you: "var(--you-stroke)", idle: "var(--stroke-2)" };
  const parts: React.ReactNode[] = [];
  spec.segs.forEach(([k, a, b], i) => {
    const x1 = X(a);
    const x2 = X(b);
    if (k === "wait")
      parts.push(
        <line key={`s${i}`} x1={x1 + 3} y1={y} x2={x2 - (mini ? 1 : 2)} y2={y} stroke={C.you} strokeWidth={mini ? 1.5 : 2} strokeDasharray={mini ? "3.5 3" : "6 5"} />,
      );
    else if (k === "waited")
      parts.push(<line key={`s${i}`} x1={x1} y1={y} x2={x2} y2={y} stroke={C.you} strokeOpacity={0.7} strokeWidth={mini ? 1.2 : 1.5} />);
    else if (k === "idle") parts.push(<line key={`s${i}`} x1={x1} y1={y} x2={x2} y2={y} stroke={C.idle} strokeWidth={1.5} />);
    else if (k === "break") {
      const m = (x1 + x2) / 2;
      parts.push(
        <path
          key={`s${i}`}
          d={`M${m - 5} ${y + 5} L${m - 1} ${y - 5} M${m + 1} ${y + 5} L${m + 5} ${y - 5}`}
          stroke="var(--text-3)"
          strokeWidth={1.5}
          strokeLinecap="round"
          fill="none"
        />,
      );
    } else parts.push(<rect key={`s${i}`} x={x1} y={y - sw / 2} width={Math.max(x2 - x1, 1)} height={sw} rx={sw / 2} fill={C[k]} />);
  });
  (spec.ticks ?? []).forEach(([k, at], i) => {
    const x = X(at);
    if (k === "ring") parts.push(<circle key={`t${i}`} cx={x} cy={y} r={mini ? 7 : 9} fill="none" stroke={C.you} strokeWidth={1.5} />);
    else if (k === "dec")
      parts.push(<circle key={`t${i}`} cx={x} cy={y} r={mini ? 2.5 : 3.5} fill={C.you} stroke="var(--tl-bg, var(--canvas))" strokeWidth={mini ? 1.5 : 2} />);
    else parts.push(<rect key={`t${i}`} x={x - tW / 2} y={y - tH / 2} width={tW} height={tH} rx={tW / 2} fill={k === "you" ? C.you : C.agent} />);
  });
  const xe = X(spec.at ?? 100);
  const k = headH / 194;
  const pointer = (pts: string, tipx: number, fill: string, stroke: string, swu: number) => (
    <g transform={`translate(${xe - tipx * k + (fill === "none" ? 3 : 1.5)} ${y - 43.3 * k - 0.8}) scale(${k})`}>
      <polygon points={pts} fill={fill} stroke={stroke} strokeWidth={swu} strokeLinejoin="round" />
    </g>
  );
  if (spec.head === "agent") parts.push(<g key="h">{pointer(PA, 120.8, "var(--agent)", "var(--agent)", 10)}</g>);
  else if (spec.head === "your-move") parts.push(<g key="h">{pointer(PY, 135.2, "none", "var(--you-stroke)", mini ? 26 : 20)}</g>);
  else if (spec.head === "you") parts.push(<g key="h">{pointer(PY, 135.2, "var(--you-stroke)", "var(--you-stroke)", 10)}</g>);
  else if (spec.head === "paused")
    parts.push(
      <foreignObject key="h" x={xe - 6} y={y - headH * 0.48} width={headH * 1.4} height={headH}>
        <SeatMark state="paused" h={headH * 0.8} />
      </foreignObject>,
    );
  else if (spec.head === "end")
    parts.push(<rect key="h" x={xe - 1} y={y - (mini ? 4 : 6)} width={2} height={mini ? 8 : 12} rx={1} fill="var(--text-3)" />);
  return (
    <svg width={w} height={h} viewBox={`0 0 ${w} ${h}`} style={{ width: w, height: h }} aria-hidden="true">
      {parts}
    </svg>
  );
}

/**
 * The turn line of a computer's feed: from its first step (or session) to now. Agent and person
 * stretches come from the driver changes, waits from the approvals.
 */
export function turnSpec(feed: FeedState, now: number, online: boolean): TurnSpec | null {
  const starts = [
    ...feed.steps.map((s) => s.startedMs),
    ...Object.values(feed.sessions).map((s) => s.since),
    ...feed.turns.map((t) => t.ts),
  ];
  if (starts.length === 0) return null;
  const t0 = Math.min(...starts);
  const span = Math.max(now - t0, 1);
  const P = (t: number) => Math.min(100, Math.max(0, ((t - t0) / span) * 100));

  // Who drove when: a list of [from, kind] changes.
  const changes: Array<[number, SegKind]> = [[t0, "agent"]];
  for (const t of feed.turns) {
    if (!t.driver) continue;
    changes.push([t.ts, t.driver.state === "person" ? "you" : t.driver.state === "paused" ? "idle" : "agent"]);
  }
  const segs: Array<[SegKind, number, number]> = [];
  changes.forEach(([at, kind], i) => {
    const end = i + 1 < changes.length ? changes[i + 1][0] : now;
    if (end > at) segs.push([kind, P(at), P(end)]);
  });
  // Approvals overlay the agent's stretch: waited (answered) or wait (open).
  for (const s of feed.steps) {
    if (!s.approvalId) continue;
    const open = s.state === "waiting";
    const end = open ? now : (s.decidedMs ?? s.startedMs);
    segs.push([open ? "wait" : "waited", P(s.startedMs), P(end)]);
  }
  const ticks: Array<[TickKind, number]> = feed.steps.map((s) => [
    s.approvalId && s.state !== "waiting" ? "dec" : s.byPerson ? "you" : "",
    P(s.decidedMs ?? s.startedMs),
  ]);
  let head: Head = "agent";
  if (!online) head = "none";
  else if (feed.pending.length) head = "your-move";
  else if (feed.driver.state === "person") head = "you";
  else if (feed.driver.state === "paused") head = "paused";
  else if (Object.keys(feed.sessions).length === 0) head = "end";
  return { segs, ticks, head };
}
