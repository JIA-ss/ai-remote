// Which OS the app runs on and which theme it shows. The design system keys radii, type sizes and
// fonts off `data-os`, and colors off `data-theme`, both on <html>.

export type Os = "windows" | "mac";
export type Theme = "light" | "dark";

export function detectOs(): Os {
  return /Mac|iPhone|iPad/.test(navigator.userAgent) ? "mac" : "windows";
}

const darkQuery = () => matchMedia("(prefers-color-scheme: dark)");

/** Applies the OS and the system theme to <html> and follows theme changes; returns a cleanup. */
export function applyPlatform(): () => void {
  const root = document.documentElement;
  root.dataset.os = detectOs();
  const sync = () => (root.dataset.theme = darkQuery().matches ? "dark" : "light");
  sync();
  const q = darkQuery();
  q.addEventListener("change", sync);
  return () => q.removeEventListener("change", sync);
}
