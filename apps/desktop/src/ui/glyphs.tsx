// Brand glyphs: the two pointers (exact logo polygons), the seat mark, the app mark, the wordmark,
// brand line glyphs on a 20 px grid, and OS glyphs. Colours come from the design tokens.

import type { CSSProperties, ReactNode } from "react";

/** Logo polygons verbatim from assets/brand/logo.svg (viewBox 0 0 256 256): the agent, then you. */
export const PA = "120.8,43.3 120.8,190.1 83.2,156.2 57.8,212.7 28.6,199.5 54.0,144.0 7.9,144.0";
export const PY = "135.2,43.3 135.2,190.1 172.8,156.2 198.2,212.7 227.4,199.5 202.0,144.0 248.1,144.0";

export type Who = "agent" | "you";

/**
 * A pointer. On the stream it carries a 1.5 px contrasting outline so it holds 3:1 over any content;
 * in the UI (`ui`) it has none and "you" uses the stroke colour that holds 3:1 on light surfaces.
 */
export function Pointer({
  who,
  h = 24,
  ui = false,
  ghost = false,
  onDark = false,
  className = "",
  style,
}: {
  who: Who;
  h?: number;
  ui?: boolean;
  ghost?: boolean;
  onDark?: boolean;
  className?: string;
  style?: CSSProperties;
}) {
  const vb = who === "agent" ? [-4, 31, 137, 194] : [123, 31, 137, 194];
  const k = h / vb[3];
  const w = vb[2] * k;
  let fill = who === "agent" ? "var(--agent)" : ui ? "var(--you-stroke)" : "var(--you)";
  if (onDark && who === "you") fill = "#F3B53F";
  const sw = ui ? 0 : (1.5 * 2) / k;
  const outline = who === "agent" ? "#FFFFFF" : "#7A4A00";
  return (
    <svg
      className={`g ptr ${className}`}
      data-kind={who}
      width={w}
      height={h}
      viewBox={vb.join(" ")}
      style={{ width: w, height: h, ...style }}
      aria-hidden="true"
    >
      <polygon
        points={who === "agent" ? PA : PY}
        fill={fill}
        opacity={ghost ? 0.28 : undefined}
        stroke={sw ? outline : undefined}
        strokeWidth={sw || undefined}
        paintOrder={sw ? "stroke" : undefined}
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** Where a pointer's tip sits inside its box, to place it by its hot spot. */
export function pointerTip(who: Who, h = 24): [number, number] {
  const k = h / 194;
  return who === "agent" ? [(120.8 + 4) * k, (43.3 - 31) * k] : [(135.2 - 123) * k, (43.3 - 31) * k];
}

/** A row's actor: the agent's or your pointer, or a small grey ring for the system. */
export function Actor({ who, h = 14 }: { who: Who | "system"; h?: number }) {
  if (who !== "system") return <Pointer who={who} h={h} ui />;
  const d = Math.round(h * 0.45);
  return (
    <span className="g actor-sys" style={{ width: h, height: h, display: "inline-grid", placeItems: "center" }}>
      <i style={{ width: d, height: d, borderRadius: "50%", boxShadow: "inset 0 0 0 1.5px var(--text-3)" }} />
    </span>
  );
}

export type Seat = "agent" | "waiting" | "asks" | "you" | "paused" | "idle" | "watch" | "both";

/**
 * The seat mark: fill = holding the controls; colour = acting or needed; grey outline = present but
 * not involved; paused or idle = two grey outlines; both solid = paired.
 */
export function SeatMark({ state, h = 28 }: { state: Seat; h?: number }) {
  const w = (h * 256) / 184;
  const sw = ((1.6 * 184) / h).toFixed(1);
  const A = ({ agent: "fill", waiting: "fill", asks: "fill", both: "fill" } as Record<string, string>)[state] ?? "out";
  const Y = ({ waiting: "need", asks: "need", you: "fill", both: "fill" } as Record<string, string>)[state] ?? "out";
  const grey = "var(--seat-grey, var(--text-3))";
  const poly = (pts: string, mode: string, color: string) =>
    mode === "fill" ? (
      <polygon points={pts} fill={color} stroke={color} strokeWidth={6} strokeLinejoin="round" />
    ) : (
      <polygon points={pts} fill="none" stroke={mode === "need" ? color : grey} strokeWidth={sw} strokeLinejoin="round" />
    );
  return (
    <svg className="g seat" viewBox="0 36 256 184" width={w} height={h} style={{ width: w, height: h, overflow: "visible" }} aria-hidden="true">
      {poly(PA, A, "var(--agent)")}
      {poly(PY, Y, "var(--seat-you, var(--you))")}
    </svg>
  );
}

/** The app mark (logo) at small sizes. */
export function Mark({ h = 16 }: { h?: number }) {
  const w = h * (256 / 184);
  return (
    <svg className="g mark" viewBox="0 36 256 184" width={w} height={h} style={{ width: w, height: h }} aria-hidden="true">
      <polygon points={PA} fill="var(--agent)" stroke="var(--agent)" strokeWidth={4} strokeLinejoin="round" />
      <polygon points={PY} fill="var(--you)" stroke="var(--you)" strokeWidth={4} strokeLinejoin="round" />
    </svg>
  );
}

const lines: Record<string, ReactNode> = {
  handoff: <path d="M3.5 7h11.5M12 4l3 3-3 3M16.5 13H5M8 10l-3 3 3 3" />,
  direct: (
    <>
      <circle cx="4" cy="10" r="2" />
      <circle cx="16" cy="10" r="2" />
      <path d="M6 10h8" />
    </>
  ),
  relayed: (
    <>
      <circle cx="3.5" cy="10" r="1.75" />
      <circle cx="16.5" cy="10" r="1.75" />
      <path d="M5.25 10h2.4M12.35 10h2.4M10 7.6l2.4 2.4-2.4 2.4-2.4-2.4z" />
    </>
  ),
  approval: (
    <>
      <path d="M10 2.6l6 2.3v4.6c0 3.8-2.5 6.4-6 7.9-3.5-1.5-6-4.1-6-7.9V4.9z" />
      <path d="M7.3 10l1.9 1.9 3.6-3.8" />
    </>
  ),
  revoke: (
    <>
      <circle cx="10" cy="10" r="7" />
      <path d="M5.1 14.9l9.8-9.8" />
    </>
  ),
  level1: (
    <>
      <path d="M4.5 15.5v-3" />
      <path d="M10 15.5v-6.5" opacity=".35" />
      <path d="M15.5 15.5V5" opacity=".35" />
    </>
  ),
  level2: (
    <>
      <path d="M4.5 15.5v-3M10 15.5v-6.5" />
      <path d="M15.5 15.5V5" opacity=".35" />
    </>
  ),
  level3: <path d="M4.5 15.5v-3M10 15.5v-6.5M15.5 15.5V5" />,
  paused: <path d="M7.5 5v10M12.5 5v10" />,
};

export type GlyphName = keyof typeof lines;

/** A brand line glyph on a 20 px grid, 1.5 px stroke. */
export function Glyph({ name, size = 16, className = "" }: { name: GlyphName; size?: number; className?: string }) {
  return (
    <svg className={`g g-stroke ${className}`} viewBox="0 0 20 20" width={size} height={size} aria-hidden="true">
      {lines[name]}
    </svg>
  );
}

export function OsGlyph({ os, size = 16 }: { os: "windows" | "mac"; size?: number }) {
  return os === "mac" ? (
    <svg className="g g-stroke" viewBox="0 0 20 20" width={size} height={size} aria-hidden="true">
      <path d="M7.5 7.5h5v5h-5zM7.5 7.5V5.75A1.75 1.75 0 1 0 5.75 7.5zM12.5 7.5V5.75a1.75 1.75 0 1 1 1.75 1.75zM7.5 12.5v1.75a1.75 1.75 0 1 1-1.75-1.75zM12.5 12.5v1.75a1.75 1.75 0 1 0 1.75-1.75z" />
    </svg>
  ) : (
    <svg className="g" viewBox="0 0 20 20" width={size} height={size} style={{ fill: "none", stroke: "currentColor", strokeWidth: 1.4 }} aria-hidden="true">
      <path d="M3.7 3.7h5.6v5.6H3.7zM10.7 3.7h5.6v5.6h-5.6zM3.7 10.7h5.6v5.6H3.7zM10.7 10.7h5.6v5.6h-5.6z" />
    </svg>
  );
}

/** The wordmark: drawn geometric paths, never typed in a font. */
export function Wordmark({ h = 28, color = "var(--text-1)" }: { h?: number; color?: string }) {
  const w = (h * 513) / 82;
  return (
    <svg className="g wordmark" viewBox="0 -82 513 82" width={w} height={h} style={{ width: w, height: h }} role="img" aria-label="ai-remote">
      <g fill="none" stroke={color} strokeWidth={13} strokeLinecap="butt" strokeLinejoin="miter">
        <path d="M6.50,-30.0 a23.5,23.5 0 1,0 47.00,0 a23.5,23.5 0 1,0 -47.00,0" />
        <path transform="translate(91 0)" d="M0,-28.0 H24" />
        <path transform="translate(124 0)" d="M6.5,-26.0 A23.5,21.5 0 0,1 30.0,-53.5 H39.0" />
        <path transform="translate(169 0)" d="M6.50,-30.0 H53.50 A23.5,23.5 0 1,0 47.46,-14.28" />
        <path transform="translate(238 0)" d="M6.5,-30.0 A19.5,23.5 0 0,1 45.5,-30.0 V0 M45.5,-30.0 A19.5,23.5 0 0,1 84.5,-30.0 V0" />
        <path transform="translate(338 0)" d="M6.50,-30.0 a23.5,23.5 0 1,0 47.00,0 a23.5,23.5 0 1,0 -47.00,0" />
        <path transform="translate(405 0)" d="M15.5,-23.5 A17,17 0 0,0 32.5,-6.5 H41.5 M0,-53.5 H36.5" />
        <path transform="translate(452.5 0)" d="M6.50,-30.0 H53.50 A23.5,23.5 0 1,0 47.46,-14.28" />
      </g>
      <g fill={color}>
        <polygon points="47.00,-60.00 60.00,-52.81 60.00,0.00 47.00,0.00" />
        <g transform="translate(69 0)">
          <polygon points="0.00,-60.00 13.00,-52.81 13.00,0.00 0.00,0.00" />
          <rect x="0" y="-82.00" width="13.0" height="13.0" />
        </g>
        <g transform="translate(124 0)">
          <polygon points="0.00,-60.00 13.00,-52.81 13.00,0.00 0.00,0.00" />
        </g>
        <g transform="translate(238 0)">
          <polygon points="0.00,-60.00 13.00,-52.81 13.00,0.00 0.00,0.00" />
        </g>
        <g transform="translate(405 0)">
          <polygon points="9.00,-80.00 22.00,-80.00 22.00,-22.90 9.00,-22.90" />
        </g>
      </g>
    </svg>
  );
}
