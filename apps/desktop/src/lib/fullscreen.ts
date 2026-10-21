// The window's full-screen state, for the session's immersive view. In the app this is the native
// macOS / Windows full screen (Tauri's `setFullscreen`); the green button and ⌃⌘F end up in the same
// state, so the page also listens for changes it did not make. In the browser demo it is a no-op.

import { api } from "./api";

const win = () => import("@tauri-apps/api/window").then((m) => m.getCurrentWindow());

export async function setFullscreen(on: boolean): Promise<void> {
  if (api.kind !== "tauri") return;
  const w = await win();
  await w.setFullscreen(on);
  // macOS: after the full-screen transition the webview sits on its last frame until it gets an
  // input event (seen on macOS 26 with Tauri 2.12: no repaint for seconds, opaque or transparent
  // window alike). Focusing the window again is enough to wake it; twice, in case the first lands
  // mid-transition.
  for (const ms of [400, 1400]) setTimeout(() => void w.setFocus().catch(() => {}), ms);
}

export async function isFullscreen(): Promise<boolean> {
  if (api.kind !== "tauri") return false;
  return (await win()).isFullscreen();
}

/** Calls `cb` with the full-screen state after a window resize; returns a cleanup. The flag can
 *  still read the old value while the macOS transition runs, so each resize is followed by three
 *  queries (0.3, 1 and 2 s later) and the last one wins. */
export function watchFullscreen(cb: (on: boolean) => void): () => void {
  if (api.kind !== "tauri") return () => {};
  let stopped = false;
  let unlisten: (() => void) | null = null;
  let timers: Array<ReturnType<typeof setTimeout>> = [];
  void win().then(async (w) => {
    if (stopped) return;
    const un = await w.onResized(() => {
      timers.forEach(clearTimeout);
      timers = [300, 1000, 2000].map((ms) => setTimeout(() => void w.isFullscreen().then((on) => !stopped && cb(on)), ms));
    });
    if (stopped) un();
    else unlisten = un;
  });
  return () => {
    stopped = true;
    timers.forEach(clearTimeout);
    unlisten?.();
  };
}
