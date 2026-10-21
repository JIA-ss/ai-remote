// The window's full-screen state, for the session's immersive view. In the app this is the native
// macOS / Windows full screen (Tauri's `setFullscreen`); the green button and ⌃⌘F end up in the same
// state, so the page also listens for changes it did not make. In the browser demo it is a no-op.

import { api } from "./api";

const win = () => import("@tauri-apps/api/window").then((m) => m.getCurrentWindow());

export async function setFullscreen(on: boolean): Promise<void> {
  if (api.kind !== "tauri") return;
  await (await win()).setFullscreen(on);
}

export async function isFullscreen(): Promise<boolean> {
  if (api.kind !== "tauri") return false;
  return (await win()).isFullscreen();
}

/** Calls `cb` with the full-screen state after every window resize; returns a cleanup. */
export function watchFullscreen(cb: (on: boolean) => void): () => void {
  if (api.kind !== "tauri") return () => {};
  let stopped = false;
  let unlisten: (() => void) | null = null;
  void win().then(async (w) => {
    if (stopped) return;
    const un = await w.onResized(() => {
      void w.isFullscreen().then((on) => !stopped && cb(on));
    });
    if (stopped) un();
    else unlisten = un;
  });
  return () => {
    stopped = true;
    unlisten?.();
  };
}
