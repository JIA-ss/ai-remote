import type { Key } from "./i18n";

/**
 * Requested encoder limits; the host display and encoder determine the actual stream (a display
 * is never upscaled, and capture runs at most at its refresh rate). Hardware encoders make frame
 * rate nearly free on the host; bitrate is what the network must carry. "Ultra" asks for the
 * display's native size and refresh, for a LAN or a fast direct path. Hosts from before
 * 2026-10-12 cap requests at 60 fps, 3840 px and 50 Mbps.
 */
export const STREAM_QUALITIES = [
  // Auto (the default): this display's refresh rate and up to 4K (see `resolveQuality`); the
  // adaptive bitrate then fits it to the network.
  { id: "auto", label: "stream_quality_auto", maxFps: 0, maxLongEdge: 0, bitrateKbps: 40000 },
  { id: "balanced", label: "stream_quality_balanced", maxFps: 60, maxLongEdge: 1920, bitrateKbps: 12000 },
  { id: "smooth", label: "stream_quality_smooth", maxFps: 120, maxLongEdge: 2560, bitrateKbps: 25000 },
  { id: "detail", label: "stream_quality_detail", maxFps: 60, maxLongEdge: 3840, bitrateKbps: 40000 },
  { id: "ultra", label: "stream_quality_ultra", maxFps: 240, maxLongEdge: 8192, bitrateKbps: 150000 },
  { id: "low", label: "stream_quality_low", maxFps: 30, maxLongEdge: 1280, bitrateKbps: 3000 },
] as const satisfies ReadonlyArray<{ id: string; label: Key; maxFps: number; maxLongEdge: number; bitrateKbps: number }>;

export type StreamQuality = (typeof STREAM_QUALITIES)[number];
export type StreamQualityId = StreamQuality["id"];

/** What a quality asks the host for, with "auto" made concrete for this display. */
export interface StreamLimits {
  maxFps: number;
  maxLongEdge: number;
  bitrateKbps: number;
}

/** Refresh rates displays commonly run at; a measurement snaps to the nearest. */
const COMMON_HZ = [60, 75, 90, 100, 120, 144, 165, 240];
let measuredHz: number | null = null;

/**
 * Counts animation frames for about a second to learn this display's refresh rate (browsers do not
 * expose it). Runs once when the app loads, long before a session opens; a hidden window gets no
 * frames and leaves the rate unknown.
 */
function measureRefresh() {
  if (typeof requestAnimationFrame !== "function" || typeof performance === "undefined") return;
  let frames = 0;
  let start = 0;
  const tick = (now: number) => {
    if (!start) start = now;
    frames++;
    if (now - start < 1000) {
      requestAnimationFrame(tick);
      return;
    }
    const hz = ((frames - 1) * 1000) / (now - start);
    if (hz >= 30) measuredHz = COMMON_HZ.reduce((best, c) => (Math.abs(c - hz) < Math.abs(best - hz) ? c : best));
  };
  requestAnimationFrame(tick);
}
measureRefresh();

/** This display's refresh rate, when it has been measured. */
export function displayHz(): number | null {
  return measuredHz;
}

export function resolveQuality(quality: StreamQuality): StreamLimits {
  if (quality.id !== "auto") return { maxFps: quality.maxFps, maxLongEdge: quality.maxLongEdge, bitrateKbps: quality.bitrateKbps };
  // An unknown refresh asks for 120: the host never sends more than its own display refreshes,
  // and the view shows the newest frame at each refresh of this one.
  const fps = Math.min(240, Math.max(60, measuredHz ?? 120));
  // This screen's width in device pixels, between 1080p and 4K: a larger picture than the screen
  // shows costs the host, the network and the decoder for nothing.
  const dpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
  const width = typeof screen !== "undefined" ? Math.round(Math.max(screen.width, screen.height) * dpr) : 1920;
  return { maxFps: fps, maxLongEdge: Math.min(3840, Math.max(1920, width)), bitrateKbps: quality.bitrateKbps };
}

const QUALITY_KEY = "ai-remote.stream-quality";

export function streamQuality(id: string | null): StreamQuality {
  return STREAM_QUALITIES.find((quality) => quality.id === id) ?? STREAM_QUALITIES[0];
}

export function loadStreamQuality(): StreamQuality {
  try {
    return streamQuality(localStorage.getItem(QUALITY_KEY));
  } catch {
    return STREAM_QUALITIES[0];
  }
}

export function saveStreamQuality(id: StreamQualityId): void {
  try {
    localStorage.setItem(QUALITY_KEY, id);
  } catch {
    // A restricted webview may disable storage; the selected limits still apply to this session.
  }
}
