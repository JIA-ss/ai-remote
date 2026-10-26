// The two-way switch at the top of the main window's sidebar: the computers (the paired computers and
// their live views) or the agents workspace. Its place and shape follow the conversation / code switch
// of the clean-room sidebar spec (SB-01): icon segments in a small track at the right of the 48 px strip.
// The last mode is remembered on this device.

import { Bot, Monitor } from "lucide-react";
import { t } from "../../lib/i18n";
import type { AppMode } from "../../lib/nav";
import { cx } from "../util";

export type { AppMode };

const MODE = "ai-remote.mode";

export function savedMode(): AppMode {
  try {
    return localStorage.getItem(MODE) === "agents" ? "agents" : "computers";
  } catch {
    return "computers";
  }
}

export function saveMode(mode: AppMode) {
  try {
    localStorage.setItem(MODE, mode);
  } catch {
    // Not remembered then.
  }
}

const MODES: Array<{ mode: AppMode; label: "ws_mode_computers" | "ws_mode_agents"; Icon: typeof Monitor }> = [
  { mode: "computers", label: "ws_mode_computers", Icon: Monitor },
  { mode: "agents", label: "ws_mode_agents", Icon: Bot },
];

/** `attention`: requests waiting on the computers side (a sentence); in the agents mode the Computers
 *  segment then carries an amber dot and the sentence in its name. */
export function ModeSwitch({ mode, onMode, attention }: { mode: AppMode; onMode: (mode: AppMode) => void; attention?: string }) {
  const pick = (m: AppMode) => {
    if (m === mode) return;
    saveMode(m);
    onMode(m);
  };
  return (
    <div
      className="ws-modes"
      role="group"
      aria-label={t("ws_mode")}
      onKeyDown={(e) => {
        if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
        e.preventDefault();
        const box = e.currentTarget;
        const next = mode === "computers" ? "agents" : "computers";
        pick(next);
        requestAnimationFrame(() => box.querySelector<HTMLButtonElement>(`[data-mode="${next}"]`)?.focus());
      }}
    >
      {MODES.map(({ mode: m, label, Icon }) => {
        const flag = !!attention && m === "computers" && mode !== "computers";
        const name = flag ? `${t(label)} · ${attention}` : t(label);
        return (
          <button
            key={m}
            type="button"
            data-mode={m}
            aria-pressed={m === mode}
            tabIndex={m === mode ? 0 : -1}
            className={cx("ws-mode", m === mode && "on")}
            aria-label={name}
            title={name}
            onClick={() => pick(m)}
          >
            <Icon size={15} strokeWidth={1.5} />
            {flag && <i className="gl-mode-dot" aria-hidden="true" />}
          </button>
        );
      })}
    </div>
  );
}
