// One keyed string catalog in English and Simplified Chinese; every screen is designed with both.
// Names from the remote screen (windows, buttons) are never translated: they are quoted as shown.

import { en } from "./strings/en";
import { zh } from "./strings/zh";

export type Lang = "en" | "zh";
export type Key = keyof typeof en;

const catalogs: Record<Lang, Record<Key, string>> = { en, zh };

export function detectLang(): Lang {
  const saved = localStorage.getItem("ai-remote.lang");
  if (saved === "en" || saved === "zh") return saved;
  return navigator.language.toLowerCase().startsWith("zh") ? "zh" : "en";
}

let lang: Lang = detectLang();

export function currentLang(): Lang {
  return lang;
}

export function setLang(next: Lang) {
  lang = next;
  localStorage.setItem("ai-remote.lang", next);
  document.documentElement.lang = next === "zh" ? "zh-Hans" : "en";
}

/** The string for `key`, with `{name}` placeholders filled from `vars`. */
export function t(key: Key, vars?: Record<string, string | number>): string {
  const s = catalogs[lang][key] ?? catalogs.en[key] ?? key;
  return vars ? s.replace(/\{(\w+)\}/g, (_, k) => String(vars[k] ?? `{${k}}`)) : s;
}
