import type { Key } from "./i18n";

/**
 * Requested encoder limits; the host display and encoder determine the actual stream (a display
 * is never upscaled, and capture runs at most at its refresh rate). Hardware encoders make frame
 * rate nearly free on the host; bitrate is what the network must carry. "Ultra" asks for the
 * display's native size and refresh, for a LAN or a fast direct path. Hosts from before
 * 2026-10-12 cap requests at 60 fps, 3840 px and 50 Mbps.
 */
export const STREAM_QUALITIES = [
  { id: "balanced", label: "stream_quality_balanced", maxFps: 60, maxLongEdge: 1920, bitrateKbps: 12000 },
  { id: "smooth", label: "stream_quality_smooth", maxFps: 120, maxLongEdge: 2560, bitrateKbps: 25000 },
  { id: "detail", label: "stream_quality_detail", maxFps: 60, maxLongEdge: 3840, bitrateKbps: 40000 },
  { id: "ultra", label: "stream_quality_ultra", maxFps: 240, maxLongEdge: 8192, bitrateKbps: 150000 },
  { id: "low", label: "stream_quality_low", maxFps: 30, maxLongEdge: 1280, bitrateKbps: 3000 },
] as const satisfies ReadonlyArray<{ id: string; label: Key; maxFps: number; maxLongEdge: number; bitrateKbps: number }>;

export type StreamQuality = (typeof STREAM_QUALITIES)[number];
export type StreamQualityId = StreamQuality["id"];

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
