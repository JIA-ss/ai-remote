// The live view's decoder: frames from the host (18-byte header + H.264 Annex B) through WebCodecs
// onto a canvas. Hardware decoding where the platform has it.

import { commands } from "../bindings";

function reportVideoError(e: unknown) {
  const message = `Live video: ${String(e)}`;
  if ("__TAURI_INTERNALS__" in window) void commands.logFromPage("error", message);
  else console.error(message);
}

export interface FrameHeader {
  keyframe: boolean;
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
  const header = {
    keyframe: (v.getUint8(1) & 1) === 1,
    width: v.getUint16(2),
    height: v.getUint16(4),
    tsUs: Number(v.getBigUint64(6)),
  };
  const len = v.getUint32(14);
  if (!header.width || !header.height || !len || len !== buf.byteLength - 18) return null;
  return { header, data: new Uint8Array(buf, 18, len) };
}

/** The `avc1.PPCCLL` codec string from the SPS in an Annex B keyframe. */
export function codecFromKeyframe(data: Uint8Array): string | null {
  for (let i = 0; i + 4 < data.length; i++) {
    const start3 = data[i] === 0 && data[i + 1] === 0 && data[i + 2] === 1;
    if (!start3) continue;
    const nal = i + 3;
    if ((data[nal] & 0x1f) === 7 && nal + 3 < data.length) {
      const hex = (b: number) => b.toString(16).padStart(2, "0");
      return `avc1.${hex(data[nal + 1])}${hex(data[nal + 2])}${hex(data[nal + 3])}`;
    }
  }
  return null;
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
  let codec: string | null = null;
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
    codec = null;
    waitingForKey = true;
    requestKeyframe();
  };

  const make = (c: string) => {
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
    // Annex B input: no `description`, the parameter sets travel in the keyframes.
    decoder.configure({ codec: c, optimizeForLatency: true, hardwareAcceleration: "prefer-hardware" });
    codec = c;
  };

  return {
    push(buf) {
      if (closed) return;
      receivedBytes += buf.byteLength;
      const f = parseFrame(buf);
      if (!f) { droppedFrames++; reset(); return; }
      if (waitingForKey && !f.header.keyframe) { droppedFrames++; requestKeyframe(); return; }
      if (f.header.keyframe) {
        const c = codecFromKeyframe(f.data);
        if (c && (c !== codec || !decoder || decoder.state === "closed")) {
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
        const currentCodec = codec;
        decoder.close();
        decoder = null;
        if (!currentCodec) { reset(); return; }
        try { make(currentCodec); }
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
