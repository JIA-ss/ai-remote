// Which OS the app runs on, which theme it shows and whether the window material is on. The design
// system keys radii, type sizes and fonts off `data-os`, colours off `data-theme`, and the glass
// layer (styles/glass) off `data-material`, all on <html>:
//
//   data-os             mac | windows
//   data-theme          light | dark (the theme shown)
//   data-theme-choice   system | light | dark (the person's choice), or pinned (`?shottheme=`)
//   data-transparency   full | reduced (Settings > General > Glass effect)
//   data-material       native (the window's own material shows through) | preview (the browser
//                       preview: a static stand-in backdrop) | none (opaque, today's look)
//   data-contrast       more | normal
//   data-window-active  true | false (the glass steps back while the window is not focused)

export type Os = "windows" | "mac";
export type Theme = "light" | "dark";
export type ThemeChoice = Theme | "system";
export type Material = "native" | "preview" | "none";

export function themeChoice(): ThemeChoice {
  const saved = localStorage.getItem("ai-remote.theme");
  return saved === "light" || saved === "dark" ? saved : "system";
}

export function reducedTransparency(): boolean {
  return localStorage.getItem("ai-remote.reduce-transparency") === "1";
}

const inApp = () => "__TAURI_INTERNALS__" in window;
const query = (q: string) => (typeof matchMedia === "function" ? matchMedia(q) : null);
const darkQuery = () => matchMedia("(prefers-color-scheme: dark)");
const MEDIA = ["(prefers-reduced-transparency: reduce)", "(prefers-contrast: more)", "(forced-colors: active)"];

/** Windows 11 or later (Mica needs it): null until the browser has answered. */
let windows11: boolean | null = null;

/** Whether the native window material is shown, the preview's stand-in, or nothing (opaque). */
export function materialOf(): Material {
  if (reducedTransparency() || MEDIA.some((q) => query(q)?.matches)) return "none";
  if (!inApp()) return "preview";
  // On Windows 10 Mica fails silently and a transparent window would show the unblurred desktop.
  if (detectOs() === "windows" && windows11 !== true) return "none";
  return "native";
}

/** The theme `?shottheme=` pins for screenshots, if any. */
function pinnedTheme(): Theme | null {
  const pinned = new URLSearchParams(location.search).get("shottheme");
  return pinned === "light" || pinned === "dark" ? pinned : null;
}

let lastNativeTheme: Theme | null | undefined;

function syncAppearance() {
  const root = document.documentElement;
  const pinned = pinnedTheme();
  const choice = themeChoice();
  root.dataset.theme = pinned ?? (choice === "system" ? (darkQuery().matches ? "dark" : "light") : choice);
  root.dataset.transparency = reducedTransparency() ? "reduced" : "full";
  root.dataset.themeChoice = pinned ? "pinned" : choice;
  root.dataset.material = materialOf();
  root.dataset.contrast = query("(prefers-contrast: more)")?.matches ? "more" : "normal";
  // The native material follows the window's appearance: pin it with the theme, so a pinned theme
  // shows the material too instead of an opaque sidebar.
  const native = pinned ?? (choice === "system" ? null : choice);
  if (inApp() && native !== lastNativeTheme) {
    lastNativeTheme = native;
    void import("@tauri-apps/api/window")
      .then(({ getCurrentWindow }) => getCurrentWindow().setTheme(native))
      .catch(() => {});
  }
}

export function setThemeChoice(choice: ThemeChoice) {
  if (choice === "system") localStorage.removeItem("ai-remote.theme");
  else localStorage.setItem("ai-remote.theme", choice);
  syncAppearance();
}

export function setReducedTransparency(reduced: boolean) {
  localStorage.setItem("ai-remote.reduce-transparency", reduced ? "1" : "0");
  syncAppearance();
}

export function detectOs(): Os {
  return /Mac|iPhone|iPad/.test(navigator.userAgent) ? "mac" : "windows";
}

/** Asks the browser for the Windows version (UA client hints: platform version 13+ is Windows 11). */
function probeWindows11(done: () => void) {
  const ua = (navigator as Navigator & { userAgentData?: { getHighEntropyValues?: (hints: string[]) => Promise<{ platformVersion?: string }> } }).userAgentData;
  if (!ua?.getHighEntropyValues) {
    windows11 = false;
    return;
  }
  ua.getHighEntropyValues(["platformVersion"])
    .then((v) => {
      windows11 = Number.parseInt(v.platformVersion ?? "0", 10) >= 13;
    })
    .catch(() => {
      windows11 = false;
    })
    .finally(done);
}

/** Follows window focus: Tauri's focus events in the app, the page's in the browser preview. */
function watchWindowActive() {
  const root = document.documentElement;
  // `?shotinactive`: screenshots of the inactive look.
  if (new URLSearchParams(location.search).has("shotinactive")) {
    root.dataset.windowActive = "false";
    return;
  }
  root.dataset.windowActive = "true";
  const set = (on: boolean) => (root.dataset.windowActive = on ? "true" : "false");
  if (inApp()) {
    void import("@tauri-apps/api/window")
      .then(({ getCurrentWindow }) => getCurrentWindow().onFocusChanged(({ payload }) => set(payload)))
      .catch(() => {});
  } else {
    window.addEventListener("focus", () => set(true));
    window.addEventListener("blur", () => set(false));
  }
}

/** Applies the OS, the theme and the material to <html> and follows their changes; returns a cleanup. */
export function applyPlatform(): () => void {
  const root = document.documentElement;
  const os = detectOs();
  root.dataset.os = os;
  // Opaque until Windows 11 is confirmed: the first frame never shows a bare desktop.
  syncAppearance();
  if (os === "windows" && inApp()) probeWindows11(syncAppearance);
  watchWindowActive();
  const queries = [darkQuery(), ...MEDIA.map(query)].filter((q): q is MediaQueryList => !!q);
  for (const q of queries) q.addEventListener("change", syncAppearance);
  return () => {
    for (const q of queries) q.removeEventListener("change", syncAppearance);
  };
}
