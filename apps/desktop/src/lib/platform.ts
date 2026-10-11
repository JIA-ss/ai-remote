// Which OS the app runs on and which theme it shows. The design system keys radii, type sizes and
// fonts off `data-os`, and colors off `data-theme`, both on <html>.

export type Os = "windows" | "mac";
export type Theme = "light" | "dark";
export type ThemeChoice = Theme | "system";

export function themeChoice(): ThemeChoice {
  const saved = localStorage.getItem("ai-remote.theme");
  return saved === "light" || saved === "dark" ? saved : "system";
}

export function reducedTransparency(): boolean {
  return localStorage.getItem("ai-remote.reduce-transparency") === "1";
}

function syncAppearance() {
  const root = document.documentElement;
  const pinned = new URLSearchParams(location.search).get("shottheme");
  const choice = themeChoice();
  root.dataset.theme = pinned === "light" || pinned === "dark" ? pinned : choice === "system" ? darkQuery().matches ? "dark" : "light" : choice;
  root.dataset.transparency = reducedTransparency() ? "reduced" : "full";
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

const darkQuery = () => matchMedia("(prefers-color-scheme: dark)");

/** Applies the OS and the system theme to <html> and follows theme changes; returns a cleanup. */
export function applyPlatform(): () => void {
  const root = document.documentElement;
  root.dataset.os = detectOs();
  syncAppearance();
  const q = darkQuery();
  q.addEventListener("change", syncAppearance);
  return () => q.removeEventListener("change", syncAppearance);
}
