// Files dropped on the app's window (the Tauri drag-drop event; HTML5 drop events are off).

export function watchFileDrop(onDrop: (paths: string[]) => void): () => void {
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return () => {};
  let stopped = false;
  let unlisten: (() => void) | null = null;
  void import("@tauri-apps/api/webview")
    .then(({ getCurrentWebview }) =>
      getCurrentWebview().onDragDropEvent((event) => {
        if (event.payload.type === "drop" && event.payload.paths.length) onDrop(event.payload.paths);
      }),
    )
    .then((u) => {
      if (stopped) u();
      else unlisten = u;
    })
    .catch(() => {});
  return () => {
    stopped = true;
    unlisten?.();
  };
}
