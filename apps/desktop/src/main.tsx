import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@fontsource/geist-mono/400.css";
import "@fontsource/geist-mono/700.css";
import "./index.css";
import App from "./App";
import { applyPlatform, detectOs } from "./lib/platform";

applyPlatform();
createRoot(document.getElementById("root") as HTMLElement).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

// Page errors go to the app's log (they are invisible otherwise).
if ("__TAURI_INTERNALS__" in window) {
  void import("./bindings").then(({ commands }) => {
    window.addEventListener("error", (e) => void commands.logFromPage("error", `${e.message} (${e.filename}:${e.lineno})`));
    window.addEventListener("unhandledrejection", (e) => void commands.logFromPage("error", `unhandled: ${String(e.reason)}`));
  });
}

// The window starts hidden. WKWebView can suspend rAF until its window is visible, so use
// the next task on macOS, after the app/styles have loaded; elsewhere wait for the first paint.
if ("__TAURI_INTERNALS__" in window) {
  const show = () => void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => getCurrentWindow().show());
  if (detectOs() === "mac") setTimeout(show, 0);
  else requestAnimationFrame(() => requestAnimationFrame(show));
}
