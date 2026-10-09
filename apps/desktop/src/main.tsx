import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@fontsource/geist-mono/400.css";
import "@fontsource/geist-mono/700.css";
import "./index.css";
import App from "./App";
import { applyPlatform } from "./lib/platform";

applyPlatform();
createRoot(document.getElementById("root") as HTMLElement).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

// The window starts hidden and is shown once the page has painted, so it never flashes white.
if ("__TAURI_INTERNALS__" in window) {
  requestAnimationFrame(() =>
    requestAnimationFrame(() => {
      void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => getCurrentWindow().show());
    }),
  );
}
