// One keyed string catalog in English and Simplified Chinese; every screen is designed with both.
// Names from the remote screen (windows, buttons) are never translated: they are quoted as shown.

import { en } from "./strings/en";
import { zh } from "./strings/zh";

export type Lang = "en" | "zh";
export type Key = keyof typeof en;

const catalogs: Record<Lang, Record<Key, string>> = { en, zh };

export function detectLang(): Lang {
  // `?shotlang=` pins the language for screenshots of the page outside the app.
  const pinned = new URLSearchParams(location.search).get("shotlang");
  if (pinned === "en" || pinned === "zh") return pinned;
  const saved = localStorage.getItem("ai-remote.lang");
  if (saved === "en" || saved === "zh") return saved;
  return navigator.language.toLowerCase().startsWith("zh") ? "zh" : "en";
}

let lang: Lang = detectLang();
document.documentElement.lang = lang === "zh" ? "zh-CN" : "en";

export function currentLang(): Lang {
  return lang;
}

/** The language the person picked here, or `system` when the app follows the system's. */
export function langChoice(): Lang | "system" {
  const saved = localStorage.getItem("ai-remote.lang");
  return saved === "en" || saved === "zh" ? saved : "system";
}

/** Follow the system's language again (takes effect on the next start or reload). */
export function followSystemLang() {
  localStorage.removeItem("ai-remote.lang");
}

export function setLang(next: Lang) {
  lang = next;
  localStorage.setItem("ai-remote.lang", next);
  document.documentElement.lang = next === "zh" ? "zh-CN" : "en";
}

/** The string for `key`, with `{name}` placeholders filled from `vars`. */
export function t(key: Key, vars?: Record<string, string | number>): string {
  const s = catalogs[lang][key] ?? catalogs.en[key] ?? key;
  if (!vars) return s;
  const filled = s.replace(/\{(\w+)\}/g, (_, k) => String(vars[k] ?? `{${k}}`));
  // Chinese templates space Latin names ("Claude Code 在等你"); a Chinese name needs no space.
  return lang === "zh" ? filled.replace(/([㐀-鿿])\s+(?=[㐀-鿿])/g, "$1") : filled;
}
