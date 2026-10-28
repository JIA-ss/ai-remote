// The model menu shared by the new-session page and the session page's composer chin (spec NS-32,
// NS-33, CP-53, CP-57): the models the agent offers (each with its one-line description), the effort
// levels the chosen model takes, and a field for a model ID typed by hand. The chip shows the model's
// name and, after it in the secondary ink, the effort word ("Opus 5.5  Extra high"). ⌘⇧I opens the
// menu, ⌘⇧E opens it at the effort levels (Ctrl+Shift elsewhere).

import { useEffect, useRef, useState, type RefObject } from "react";
import type { ModelInfo } from "../../lib/chat";
import { t, type Key } from "../../lib/i18n";
import { effortsFor, modelEntry, modelName } from "../../lib/workspace";
import { cx } from "../util";
import { isMac, workspaceOwnsEvent, type MenuLine } from "./parts";

/** An effort level in words (one Codex may add that has no words here is shown as it is). */
export function effortWord(effort: string): string {
  const key = `ws_effort_${effort}` as Key;
  const w = t(key);
  return w === key ? effort : w;
}

/** Which part of the menu has the focus when it opens: the model (⌘⇧I) or the effort level (⌘⇧E). */
export type ModelFocus = "model" | "effort";

/**
 * The menu's lines: a heading, the default model, every model offered (and the one in use when it was
 * typed by hand), models typed before for this agent, then the effort levels the model takes (NS-33).
 */
export function modelMenuLines(p: {
  heading: string;
  models: readonly ModelInfo[];
  protocol: string | undefined;
  model: string;
  effort: string | null;
  typed?: readonly string[];
  focus?: ModelFocus;
  onModel: (model: string) => void;
  onEffort: (effort: string | null) => void;
}): MenuLine[] {
  const { models, model, effort } = p;
  const def = modelEntry(models, "");
  const lines: MenuLine[] = [{ heading: p.heading }];
  lines.push({
    key: "m:",
    label: def ? t("ws_model_default_named", { name: def.name }) : t("ws_model_default"),
    detail: def?.description ?? t("ws_model_default_d"),
    checked: !model,
    run: () => p.onModel(""),
  });
  for (const m of models) {
    lines.push({ key: `m:${m.id}`, label: m.name, detail: m.description, checked: model === m.id, run: () => p.onModel(m.id) });
  }
  const offered = (id: string) => models.some((m) => m.id === id);
  const typed = (p.typed ?? []).filter((m) => !offered(m));
  if (model && !offered(model) && !typed.includes(model)) {
    lines.push({ key: `c:${model}`, label: model, checked: true, run: () => {} });
  }
  if (typed.length) {
    lines.push({ heading: t("ws_model_recent") });
    for (const m of typed) lines.push({ key: `t:${m}`, label: m, checked: model === m, run: () => p.onModel(m) });
  }
  lines.push("sep", { heading: t("ws_effort") });
  const levels = effortsFor(models, model, p.protocol);
  if (!levels.length) {
    lines.push({ key: "e:none", label: t("ws_effort_none_for"), disabled: true, run: () => {} });
    return lines;
  }
  const own = modelEntry(models, model)?.default_effort;
  lines.push({
    key: "e:",
    label: own && levels.includes(own) ? t("ws_effort_default_named", { name: effortWord(own) }) : t("ws_effort_default"),
    checked: !effort,
    autofocus: p.focus === "effort" && !effort,
    run: () => p.onEffort(null),
  });
  for (const e of levels) {
    lines.push({ key: `e:${e}`, label: effortWord(e), checked: effort === e, autofocus: p.focus === "effort" && effort === e, run: () => p.onEffort(e) });
  }
  return lines;
}

/** The chip's words: the model's name (the default's when none is chosen) and the effort word after it. */
export function ModelChipText({ models, protocol, model, effort }: { models: readonly ModelInfo[]; protocol: string | undefined; model: string; effort: string | null }) {
  const name = modelName(models, model, protocol) || t("ws_model_default");
  return (
    <>
      <span className="ws-chip-text">{name}</span>
      {effort && (
        <span className="ws-chip-sub" aria-label={t("ws_effort_aria", { name: effortWord(effort) })}>
          {effortWord(effort)}
        </span>
      )}
    </>
  );
}

/** The chip's class: accent when a model or level was chosen (NS-32). */
export const modelChipClass = (model: string, effort: string | null) => cx("ws-chip ws-model-chip", (model || effort) && "set");

/** The shortcut's words for the chip's tooltip. */
export const modelKeys = isMac ? "⌘⇧I" : "Ctrl+Shift+I";

/**
 * ⌘⇧I / ⌘⇧E (Ctrl+Shift elsewhere) while `scope` owns the keyboard: `open` is told which part to
 * focus. Not while composing, not on a held key.
 */
export function useModelKeys(scope: RefObject<HTMLElement | null>, enabled: boolean, open: (focus: ModelFocus) => void) {
  const opener = useRef(open);
  opener.current = open;
  useEffect(() => {
    if (!enabled) return;
    const on = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.isComposing || e.repeat) return;
      const chord = isMac ? e.metaKey && e.shiftKey && !e.ctrlKey && !e.altKey : e.ctrlKey && e.shiftKey && !e.metaKey && !e.altKey;
      if (!chord || (e.code !== "KeyI" && e.code !== "KeyE")) return;
      const root = scope.current;
      if (!root || !root.isConnected) return;
      const pane = root.closest<HTMLElement>('.ws[data-embedded="true"]');
      const mine = pane ? workspaceOwnsEvent(pane, e) : e.target instanceof Node && (root.contains(e.target) || e.target === document.body);
      if (!mine) return;
      e.preventDefault();
      e.stopPropagation();
      opener.current(e.code === "KeyE" ? "effort" : "model");
    };
    window.addEventListener("keydown", on, true);
    return () => window.removeEventListener("keydown", on, true);
  }, [enabled, scope]);
}

/** The menu's last line: a model ID typed by hand, used with Enter ("" is the agent's default). */
export function ModelField({ initial, codex, first, onDone }: { initial: string; codex: boolean; first: boolean; onDone: (model: string) => void }) {
  const [v, setV] = useState(initial);
  return (
    <>
      <div className="ws-menu-sep" role="separator" />
      <div className="ws-menu-field">
        <input
          className="mono"
          value={v}
          data-autofocus={first || undefined}
          placeholder={codex ? "gpt-…" : t("ws_model_other_ph")}
          aria-label={t("ws_model_other")}
          spellCheck={false}
          onChange={(e) => setV(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              e.stopPropagation();
              onDone(v.trim());
            }
          }}
        />
      </div>
    </>
  );
}
