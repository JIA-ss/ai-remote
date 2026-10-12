// The live view's decoder: frames from the host (18-byte header + H.264/HEVC Annex B or AV1 OBUs)
// through WebCodecs onto a canvas. Hardware decoding where the platform has it.

import { commands } from "../bindings";
import { codecOfFlags, codecString, type VideoCodec } from "./codec";

function reportVideoError(e: unknown) {
  const message = `Live video: ${String(e)}`;
  if ("__TAURI_INTERNALS__" in window) void commands.logFromPage("error", message);
  else console.error(message);
}

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
      droppedFrames, decodeQueueSize: decoder?.decodeQueueSize ?? 0 });
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
        try { ctx?.drawImage(frame, 0, 0); decodedFrames++; }
        finally { frame.close(); }
      },
      error: (e) => {
        if (closed || decoder !== next) return;
        reportVideoError(e);
        reset();
      },
    });
    decoder = next;
    // Annex B / OBU input: no `description`, the parameter sets travel in the keyframes.
    decoder.configure(c);
    config = c;
  };

  return {
    push(buf) {
      if (closed) return;
      receivedBytes += buf.byteLength;
      const f = parseFrame(buf);
      if (!f) { droppedFrames++; reset(); return; }
      if (waitingForKey && !f.header.keyframe) { droppedFrames++; requestKeyframe(); return; }
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
          catch (e) { reportVideoError(e); droppedFrames++; reset(); return; }
        }
        if (decoder?.state === "configured") waitingForKey = false;
      }
      if (!decoder || decoder.state !== "configured") return;
      // Behind by too much (a slow machine): drop to the next keyframe instead of lagging.
      if (decoder.decodeQueueSize >= 2 && !f.header.keyframe) {
        droppedFrames += decoder.decodeQueueSize + 1;
        reset();
        return;
      }
      if (decoder.decodeQueueSize >= 2 && f.header.keyframe) {
        droppedFrames += decoder.decodeQueueSize;
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
      } catch (e) { reportVideoError(e); droppedFrames++; reset(); }
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
