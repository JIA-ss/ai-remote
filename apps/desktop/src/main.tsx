import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@fontsource/geist-mono/400.css";
import "@fontsource/geist-mono/700.css";
import "./index.css";
import App from "./App";
import { api } from "./lib/api";
import { probeCodecs } from "./lib/codec";
import { currentLang } from "./lib/i18n";
import { applyPlatform, detectOs } from "./lib/platform";

applyPlatform();
// The tray menu reads in the page's language (a change reloads the page, so this runs again).
void api.setUiLanguage(currentLang()).catch(() => {});
// Which codecs this machine decodes in hardware decides what live views ask hosts for.
void probeCodecs();
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
// Started by the login item (`--hidden`), the app stays in the tray until someone opens it there.
if ("__TAURI_INTERNALS__" in window) {
  const show = () => void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => getCurrentWindow().show());
  void import("./bindings").then(({ commands }) => commands.startedHidden()).catch(() => false).then((hidden) => {
    if (hidden) return;
    if (detectOs() === "mac") setTimeout(show, 0);
    else requestAnimationFrame(() => requestAnimationFrame(show));
  });
}
