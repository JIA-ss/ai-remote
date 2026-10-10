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
  return { header, data: new Uint8Array(buf, 18, Math.min(len, buf.byteLength - 18)) };
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
): LiveDecoder {
  const ctx = canvas.getContext("2d", { alpha: false, desynchronized: true });
  let decoder: VideoDecoder | null = null;
  let codec: string | null = null;
  let waitingForKey = true;

  const reset = () => {
    try {
      decoder?.close();
    } catch {
      /* already closed */
    }
    decoder = null;
    codec = null;
    waitingForKey = true;
    onNeedKeyframe();
  };

  const make = (c: string) => {
    decoder = new VideoDecoder({
      output: (frame) => {
        if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
          canvas.width = frame.displayWidth;
          canvas.height = frame.displayHeight;
          onSize?.(frame.displayWidth, frame.displayHeight);
        }
        ctx?.drawImage(frame, 0, 0);
        frame.close();
      },
      error: (e) => {
        reportVideoError(e);
        reset();
      },
    });
    // Annex B input: no `description`, the parameter sets travel in the keyframes.
    decoder.configure({ codec: c, optimizeForLatency: true, hardwareAcceleration: "prefer-hardware" });
    codec = c;
  };

  return {
    push(buf) {
      const f = parseFrame(buf);
      if (!f) return;
      if (waitingForKey && !f.header.keyframe) return;
      if (f.header.keyframe) {
        const c = codecFromKeyframe(f.data);
        if (c && (c !== codec || !decoder || decoder.state === "closed")) {
          try {
            decoder?.close();
          } catch {
            /* already closed */
          }
          make(c);
        }
        waitingForKey = false;
      }
      if (!decoder || decoder.state !== "configured") return;
      // Behind by too much (a slow machine): drop to the next keyframe instead of lagging.
      if (decoder.decodeQueueSize > 8 && !f.header.keyframe) {
        reset();
        return;
      }
      decoder.decode(
        new EncodedVideoChunk({ type: f.header.keyframe ? "key" : "delta", timestamp: f.header.tsUs, data: f.data }),
      );
    },
    close() {
      try {
        decoder?.close();
      } catch {
        /* already closed */
      }
      decoder = null;
    },
  };
}
