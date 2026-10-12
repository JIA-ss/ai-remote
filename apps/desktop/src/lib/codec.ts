// Codecs of the live view. A frame header carries its codec (flags bits 1-2); the decoder needs a
// WebCodecs codec string for it; the request tells the host which codecs this machine decodes in
// hardware, best first. H.264 is always last: every host can send it, in software if need be.

export type VideoCodec = "h264" | "hevc" | "av1";

const BY_WIRE_ID: VideoCodec[] = ["h264", "hevc", "av1"];

/** The codec in a frame header's flags byte, or null for an id this viewer does not know. */
export function codecOfFlags(flags: number): VideoCodec | null {
  return BY_WIRE_ID[(flags >> 1) & 3] ?? null;
}

/** The `avc1.PPCCLL` string from the SPS in an Annex B keyframe. */
export function avcCodecString(data: Uint8Array): string | null {
  for (let i = 0; i + 4 < data.length; i++) {
    if (data[i] !== 0 || data[i + 1] !== 0 || data[i + 2] !== 1) continue;
    const nal = i + 3;
    if ((data[nal] & 0x1f) === 7 && nal + 3 < data.length) {
      const hex = (b: number) => b.toString(16).padStart(2, "0");
      return `avc1.${hex(data[nal + 1])}${hex(data[nal + 2])}${hex(data[nal + 3])}`;
    }
  }
  return null;
}

// HEVC Main and AV1 Main, 8-bit 4:2:0, at a level high enough for 8K: the decoder reads the real
// parameters from the in-band VPS/SPS or sequence header; the string only selects the decoder.
const HEVC = "hev1.1.6.L186.B0";
const AV1 = "av01.0.16M.08";

/** The WebCodecs codec string for a keyframe of `codec`. */
export function codecString(codec: VideoCodec, keyframe: Uint8Array): string | null {
  if (codec === "h264") return avcCodecString(keyframe);
  return codec === "hevc" ? HEVC : AV1;
}

/**
 * In order of preference. HEVC first: its hardware encoders keep to the requested bitrate at every
 * size measured, encode 4K in half of H.264's time and take sizes past 4096, and almost every GPU
 * and Apple device decodes it. NVENC's AV1 encodes faster still but does not keep to the requested
 * bitrate (the host moves away from it when that shows); it stays as the choice when HEVC is
 * missing. Measured 2026-10-12, see AGENTS.md.
 */
const PROBES: Array<[VideoCodec, string]> = [["hevc", HEVC], ["av1", AV1], ["h264", "avc1.640034"]];

/** Codecs this machine decodes in hardware (probed once, at 4K), best first, always ending with H.264. */
export function probeCodecs(): Promise<VideoCodec[]> {
  probed ??= probe(3840, 2160).then((codecs) => (known = codecs));
  return probed;
}

/** What to ask a host for now: the probe's result (started at app launch), H.264 until it ends. */
export function preferredCodecs(): VideoCodec[] {
  void probeCodecs();
  return known;
}

let probed: Promise<VideoCodec[]> | null = null;
let known: VideoCodec[] = ["h264"];

async function probe(width: number, height: number): Promise<VideoCodec[]> {
  const out: VideoCodec[] = [];
  if (typeof VideoDecoder === "undefined") return ["h264"];
  for (const [codec, string] of PROBES) {
    if (codec === "h264") continue;
    try {
      const { supported } = await VideoDecoder.isConfigSupported({
        codec: string, codedWidth: width, codedHeight: height, hardwareAcceleration: "prefer-hardware",
      });
      if (supported) out.push(codec);
    } catch {
      // An engine without this codec rejects the probe; H.264 remains.
    }
  }
  out.push("h264");
  return out;
}
