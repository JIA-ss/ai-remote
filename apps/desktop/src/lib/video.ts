// The live view's decoder: frames from the host (18-byte header + H.264/HEVC Annex B or AV1 OBUs)
// through WebCodecs onto a canvas. Hardware decoding where the platform has it.

import { commands } from "../bindings";
import { codecOfFlags, codecString, type VideoCodec } from "./codec";

function logVideo(level: "error" | "info", text: string) {
  const message = `Live video: ${text}`;
  if ("__TAURI_INTERNALS__" in window) void commands.logFromPage(level, message);
  else if (level === "error") console.error(message);
  else console.info(message);
}

function reportVideoError(e: unknown) {
  logVideo("error", String(e));
}

/** Why frames were not shown: unparsable, behind the decoder, decoder errors, or waiting for a
 * keyframe after one of those (or before the first one). */
export type DropReason = "invalid" | "backlog" | "error" | "awaitingKeyframe";

export interface FrameHeader {
  keyframe: boolean;
  codec: VideoCodec;
  width: number;
  height: number;
  /** Microseconds since the stream started. */
  tsUs: number;
}

export interface VideoStats {
  width: number;
  height: number;
  decodedFps: number;
  receivedKbps: number;
  droppedFrames: number;
  /** `droppedFrames` by reason, since the stream started. */
  drops: Record<DropReason, number>;
  decodeQueueSize: number;
}

export function parseFrame(buf: ArrayBuffer): { header: FrameHeader; data: Uint8Array } | null {
  if (buf.byteLength < 18) return null;
  const v = new DataView(buf);
  if (v.getUint8(0) !== 1) return null;
  const codec = codecOfFlags(v.getUint8(1));
  if (!codec) return null;
  const header = {
    keyframe: (v.getUint8(1) & 1) === 1,
    codec,
    width: v.getUint16(2),
    height: v.getUint16(4),
    tsUs: Number(v.getBigUint64(6)),
  };
  const len = v.getUint32(14);
  if (!header.width || !header.height || !len || len !== buf.byteLength - 18) return null;
  return { header, data: new Uint8Array(buf, 18, len) };
}

/**
 * When the view gives up on a lagging decoder and drops to the next keyframe. Lag is the stream
 * time between the oldest frame still waiting to be decoded and the newest one: time, not a frame
 * count, so it means the same at 30 and 240 fps from the first frame. A hardware decoder keeps up
 * (300+ fps at 5K) and clears a stall in tens of milliseconds, while dropping costs every frame
 * until a keyframe arrives, and that keyframe (megabytes at 4K/5K) can cause the next stall: a
 * two-frame limit made a 5K/100 fps view reset four times in half a second and drop 89 frames in
 * 12 s (2026-10-12). So a lag of `BACKLOG_US` is ridden out unless it lasts `BACKLOG_PERSIST_MS`
 * (a decoder slower than the stream), or reaches `BACKLOG_HARD_US`.
 */
const BACKLOG_US = 150_000;
const BACKLOG_PERSIST_MS = 300;
const BACKLOG_HARD_US = 1_000_000;

export interface LiveDecoder {
  push(buf: ArrayBuffer): void;
  close(): void;
}

/**
 * Decodes frames onto `canvas`. `onNeedKeyframe` is called when the decoder lost its place (start,
 * error); `onSize` when the stream size changes.
 */
export function createDecoder(
  canvas: HTMLCanvasElement,
  onNeedKeyframe: () => void,
  onSize?: (w: number, h: number) => void,
  onStats?: (stats: VideoStats) => void,
): LiveDecoder {
  const ctx = canvas.getContext("2d", { alpha: false, desynchronized: true });
  let decoder: VideoDecoder | null = null;
  // The configuration the decoder was made with: codec string and coded size. Chromium's HEVC
  // path reports a 1280x720 display size unless the coded size is given (seen 2026-10-12).
  let config: VideoDecoderConfig | null = null;
  let waitingForKey = true;
  let closed = false;
  let lastKeyframeRequest = -Infinity;
  let intervalStart = performance.now();
  let receivedBytes = 0;
  let decodedFrames = 0;
  let width = 0, height = 0;
  let droppedFrames = 0;
  const drops: Record<DropReason, number> = { invalid: 0, backlog: 0, error: 0, awaitingKeyframe: 0 };
  const drop = (reason: DropReason, n = 1) => {
    droppedFrames += n;
    drops[reason] += n;
  };
  const started = performance.now();
  let totalDecoded = 0;
  // Stream timestamps of the frames given to the current decoder, newest last (a bounded window).
  let submitted: number[] = [];
  // How far the decoder is behind `newest`: from the oldest frame still queued to it, in us.
  const lag = (queued: number, newest: number) =>
    queued > 0 && submitted.length > 0 ? newest - submitted[Math.max(0, submitted.length - queued)] : 0;
  // The current decoder has caught up once: shown a frame with its queue within the allowance. A
  // hardware decoder takes 300-700 ms to start (WebView2, 2026-10-12) and the frames that queue
  // meanwhile drain right after; judging them as a backlog dropped 11-20 frames at every start.
  let caughtUp = false;
  let createdAt = 0;
  // Since when the decoder has been at least `BACKLOG_US` behind, if it is.
  let behindSince: number | null = null;

  const requestKeyframe = () => {
    const now = performance.now();
    if (now - lastKeyframeRequest >= 250) {
      lastKeyframeRequest = now;
      onNeedKeyframe();
    }
  };
  const metricsTimer = window.setInterval(() => {
    if (closed) return;
    const now = performance.now();
    const seconds = Math.max((now - intervalStart) / 1000, .001);
    onStats?.({ width, height, decodedFps: decodedFrames / seconds, receivedKbps: receivedBytes * 8 / seconds / 1000,
      droppedFrames, drops: { ...drops }, decodeQueueSize: decoder?.decodeQueueSize ?? 0 });
    intervalStart = now;
    receivedBytes = decodedFrames = 0;
  }, 1000);

  const reset = () => {
    try {
      decoder?.close();
    } catch {
      /* already closed */
    }
    decoder = null;
    config = null;
    waitingForKey = true;
    requestKeyframe();
  };

  const make = (c: VideoDecoderConfig) => {
    const next = new VideoDecoder({
      output: (frame) => {
        if (closed || decoder !== next) { frame.close(); return; }
        width = frame.displayWidth;
        height = frame.displayHeight;
        if (canvas.width !== width || canvas.height !== height) {
          canvas.width = width;
          canvas.height = height;
          onSize?.(frame.displayWidth, frame.displayHeight);
        }
        if (!caughtUp && lag(next.decodeQueueSize, submitted.at(-1) ?? 0) < BACKLOG_US) {
          caughtUp = true;
          logVideo("info", `decoder caught up ${Math.round(performance.now() - createdAt)} ms after it started`);
        }
        try { ctx?.drawImage(frame, 0, 0); decodedFrames++; totalDecoded++; }
        finally { frame.close(); }
      },
      error: (e) => {
        if (closed || decoder !== next) return;
        reportVideoError(e);
        reset();
      },
    });
    decoder = next;
    caughtUp = false;
    createdAt = performance.now();
    behindSince = null;
    submitted = [];
    // Annex B / OBU input: no `description`, the parameter sets travel in the keyframes.
    decoder.configure(c);
    config = c;
  };

  return {
    push(buf) {
      if (closed) return;
      receivedBytes += buf.byteLength;
      const f = parseFrame(buf);
      if (!f) { drop("invalid"); reset(); return; }
      if (waitingForKey && !f.header.keyframe) { drop("awaitingKeyframe"); requestKeyframe(); return; }
      if (f.header.keyframe) {
        const string = codecString(f.header.codec, f.data);
        const c: VideoDecoderConfig | null = string
          ? { codec: string, codedWidth: f.header.width, codedHeight: f.header.height,
            optimizeForLatency: true, hardwareAcceleration: "prefer-hardware" }
          : null;
        const changed = c && (c.codec !== config?.codec || c.codedWidth !== config?.codedWidth
          || c.codedHeight !== config?.codedHeight);
        if (c && (changed || !decoder || decoder.state === "closed")) {
          try {
            decoder?.close();
          } catch {
            /* already closed */
          }
          try { make(c); }
          catch (e) { reportVideoError(e); drop("error"); reset(); return; }
        }
        if (decoder?.state === "configured") waitingForKey = false;
      }
      if (!decoder || decoder.state !== "configured") return;
      // Behind by too much (a slow machine): drop to the next keyframe instead of lagging.
      // A decoder that has not caught up 2 s after starting is judged like any other.
      const judged = caughtUp || performance.now() - createdAt > 2000;
      const behindUs = lag(decoder.decodeQueueSize, f.header.tsUs);
      const lagging = judged && behindUs >= BACKLOG_US;
      const now = performance.now();
      behindSince = lagging ? behindSince ?? now : null;
      // A keyframe replaces the queue at no extra cost; otherwise only a lasting or large lag drops.
      const behind = lagging
        && (f.header.keyframe || behindUs >= BACKLOG_HARD_US || now - (behindSince ?? now) >= BACKLOG_PERSIST_MS);
      if (behind && !f.header.keyframe) {
        logVideo("info", `decoder ${Math.round(behindUs / 1000)} ms (${decoder.decodeQueueSize} frames) behind ${Math.round(performance.now() - started)} ms into the stream (${totalDecoded} shown); dropping to the next keyframe`);
        drop("backlog", decoder.decodeQueueSize + 1);
        reset();
        return;
      }
      if (behind && f.header.keyframe) {
        drop("backlog", decoder.decodeQueueSize);
        // An IDR is independent of the discarded queue. Recreate immediately with
        // this keyframe instead of waiting for another desktop change.
        const current = config;
        decoder.close();
        decoder = null;
        if (!current) { reset(); return; }
        try { make(current); }
        catch (e) { reportVideoError(e); reset(); return; }
      }
      try {
        if (!decoder || decoder.state !== "configured") return;
        decoder.decode(new EncodedVideoChunk({ type: f.header.keyframe ? "key" : "delta", timestamp: f.header.tsUs, data: f.data }));
        submitted.push(f.header.tsUs);
        if (submitted.length > 1024) submitted = submitted.slice(-512);
      } catch (e) { reportVideoError(e); drop("error"); reset(); }
    },
    close() {
      closed = true;
      clearInterval(metricsTimer);
      try {
        decoder?.close();
      } catch {
        /* already closed */
      }
      decoder = null;
    },
  };
}
