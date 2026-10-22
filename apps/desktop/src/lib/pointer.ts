// The host's pointer, drawn here over the video when the host sends it separately (see
// `PointerPosition` in api.ts): moving it then costs the host a few bytes instead of a re-encoded
// frame, and it moves as often as the host looks at it. Positions arrive at up to the stream's
// frame rate and only move a canvas (a CSS transform), so React is not involved.

import type { PointerPosition, PointerShape } from "./api";

export interface PointerLayer {
  shape(shape: PointerShape): void;
  /** `screen` is the streamed monitor's size in the units of `at`. Returns whether the pointer is
   *  drawn now (false while the host reports it hidden or off this monitor). */
  move(at: PointerPosition, screen: { width: number; height: number }): boolean;
  hide(): void;
}

type Picture = { width: number; height: number; hotX: number; hotY: number; data: ImageData | null };

/** Where shape 0 (the host could not read the pointer) or a shape that never came is drawn. */
const ARROW: Picture = { width: 13, height: 20, hotX: 0, hotY: 0, data: null };

function drawArrow(ctx: CanvasRenderingContext2D) {
  ctx.beginPath();
  ctx.moveTo(0.5, 0.5);
  ctx.lineTo(0.5, 16.5);
  ctx.lineTo(4.5, 12.5);
  ctx.lineTo(7.5, 19);
  ctx.lineTo(10, 18);
  ctx.lineTo(7, 11.5);
  ctx.lineTo(12.5, 11.5);
  ctx.closePath();
  ctx.fillStyle = "#000";
  ctx.strokeStyle = "#fff";
  ctx.fill();
  ctx.stroke();
}

/** Draws on `layer` (absolutely positioned over `video`, which shows the whole streamed monitor). */
export function createPointerLayer(layer: HTMLCanvasElement, video: HTMLElement): PointerLayer {
  const pictures = new Map<number, Picture>();
  let drawn: Picture | null = null;
  const ctx = layer.getContext("2d");
  const hide = () => {
    layer.style.display = "none";
  };
  hide();
  return {
    shape(s) {
      pictures.set(s.id, { width: s.width, height: s.height, hotX: s.hotX, hotY: s.hotY,
        data: new ImageData(new Uint8ClampedArray(s.rgba), s.width, s.height) });
      // A shape that changed under the same id is redrawn on the next move.
      if (drawn && pictures.get(s.id) !== drawn) drawn = null;
    },
    move(at, screen) {
      if (!at.visible || at.x < 0 || at.y < 0 || at.x >= screen.width || at.y >= screen.height || !ctx) {
        hide();
        return false;
      }
      const picture = pictures.get(at.shape) ?? ARROW;
      if (picture !== drawn) {
        layer.width = picture.width;
        layer.height = picture.height;
        ctx.clearRect(0, 0, picture.width, picture.height);
        if (picture.data) ctx.putImageData(picture.data, 0, 0);
        else drawArrow(ctx);
        drawn = picture;
      }
      // Screen units to CSS pixels: the video element is the whole monitor, stretched.
      const scale = video.clientWidth / screen.width;
      // Shape 0 is drawn at CSS size: it is ours, not a picture from the host's screen.
      const size = picture === ARROW ? 1 : scale;
      layer.style.width = `${picture.width * size}px`;
      layer.style.height = `${picture.height * size}px`;
      layer.style.transform =
        `translate(${video.offsetLeft + at.x * scale - picture.hotX * size}px, ${video.offsetTop + at.y * scale - picture.hotY * size}px)`;
      layer.style.display = "block";
      return true;
    },
    hide,
  };
}
