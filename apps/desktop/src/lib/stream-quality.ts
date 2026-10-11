import type { Key } from "./i18n";

/** Requested encoder limits; the host display and encoder determine the actual stream. */
export const STREAM_QUALITIES = [
  { id: "balanced", label: "stream_quality_balanced", maxFps: 30, maxLongEdge: 1920, bitrateKbps: 6000 },
  { id: "smooth", label: "stream_quality_smooth", maxFps: 60, maxLongEdge: 1920, bitrateKbps: 10000 },
  { id: "detail", label: "stream_quality_detail", maxFps: 30, maxLongEdge: 3840, bitrateKbps: 20000 },
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
