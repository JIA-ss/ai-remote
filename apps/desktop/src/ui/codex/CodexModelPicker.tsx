// The Codex composer's model and reasoning-effort picker (XNT-35, XNT-36, XNT-37, XCP-40): a quiet
// button under the field with the model's name (no "GPT-" prefix), the effort it works at and a down
// arrow; it opens the shared model menu (the models Codex offers, then the effort levels the chosen
// one takes, then a field for a model ID). A choice goes to the host as a `model` command; Codex takes
// it with the next turn, and the button shows it once the host says it holds (a `model` event).
// Ctrl+Shift+M opens it, as in Codex's app (Control on a Mac too); ⌘⇧I / ⌘⇧E as everywhere here.

import { ChevronDown } from "lucide-react";
import { useEffect, useRef, useState, type RefObject } from "react";
import type { ModelInfo } from "../../lib/chat";
import { codexModelWords } from "../../lib/codex";
import { t } from "../../lib/i18n";
import { keepEffort, rememberModel } from "../../lib/workspace";
import { effortWord, ModelField, modelKeys, modelMenuLines, useModelKeys, type ModelFocus } from "../workspace/ModelMenu";
import { PopMenu, workspaceOwnsEvent, type MenuAt } from "../workspace/parts";

/** Models typed for each agent (by runtime id), newest first: the list the other pickers keep too. */
const TYPED = "ai-remote.ws.models";

function typedFor(runtime: string): string[] {
  try {
    return (JSON.parse(localStorage.getItem(TYPED) ?? "{}") as Record<string, string[]>)[runtime] ?? [];
  } catch {
    return [];
  }
}

function rememberTyped(runtime: string, model: string, protocol: string | undefined) {
  try {
    const all = JSON.parse(localStorage.getItem(TYPED) ?? "{}") as Record<string, string[]>;
    localStorage.setItem(TYPED, JSON.stringify({ ...all, [runtime]: rememberModel(all[runtime] ?? [], model, protocol) }));
  } catch {
    // Kept for this run only.
  }
}

/** Ctrl+Shift+M (Control on every system, as Codex binds it) while `scope` owns the keyboard. */
function useCodexPickerKey(scope: RefObject<HTMLElement | null>, enabled: boolean, open: () => void) {
  const opener = useRef(open);
  opener.current = open;
  useEffect(() => {
    if (!enabled) return;
    const on = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.isComposing || e.repeat) return;
      if (!e.ctrlKey || !e.shiftKey || e.metaKey || e.altKey || e.code !== "KeyM") return;
      const root = scope.current;
      if (!root || !root.isConnected) return;
      const pane = root.closest<HTMLElement>('.ws[data-embedded="true"]');
      const mine = pane ? workspaceOwnsEvent(pane, e) : e.target instanceof Node && (root.contains(e.target) || e.target === document.body);
      if (!mine) return;
      e.preventDefault();
      e.stopPropagation();
      opener.current();
    };
    window.addEventListener("keydown", on, true);
    return () => window.removeEventListener("keydown", on, true);
  }, [enabled, scope]);
}

export function CodexModelPicker({ scope, runtime, protocol, models, model, effort, disabled, onPick }: {
  scope: RefObject<HTMLElement | null>;
  runtime: string;
  protocol: string | undefined;
  models: readonly ModelInfo[];
  /** The model and level in use ("" / null: Codex's own default). */
  model: string;
  effort: string | null;
  disabled: boolean;
  onPick: (model: string, effort: string | null) => void;
}) {
  const [menu, setMenu] = useState<{ at: MenuAt; focus?: ModelFocus } | null>(null);
  const [btn, setBtn] = useState<HTMLButtonElement | null>(null);
  const open = (focus?: ModelFocus) => {
    if (btn) setMenu({ at: { anchor: btn.getBoundingClientRect(), side: "above" }, focus });
  };
  useModelKeys(scope, !disabled, open);
  useCodexPickerKey(scope, !disabled, () => (menu ? setMenu(null) : open()));
  const typed = typedFor(runtime);
  const pickModel = (m: string) => {
    if (m && !models.some((x) => x.id === m)) rememberTyped(runtime, m, protocol);
    // A level the new model does not take is not carried over; the host then uses the model's own (XNT-37).
    if (m !== model) onPick(m, keepEffort(models, m, effort, protocol));
  };
  const words = codexModelWords(models, model, effort);
  const custom = !!model && !models.some((m) => m.id === model);
  return (
    <span className="cdx-model">
      <button
        ref={setBtn}
        type="button"
        className="x-model"
        disabled={disabled}
        aria-haspopup="menu"
        aria-expanded={!!menu}
        title={t("cdx_model_tip", { key: modelKeys })}
        onClick={(e) => {
          e.stopPropagation();
          if (menu) setMenu(null);
          else open();
        }}
      >
        <span className="x-model-name">{words.name || t("ws_model_default")}</span>
        {words.effort && (
          <span className="x-model-effort" aria-label={t("ws_effort_aria", { name: effortWord(words.effort) })}>
            {effortWord(words.effort)}
          </span>
        )}
        <ChevronDown size={14} className="x-model-arrow" aria-hidden />
      </button>
      {menu && (
        <PopMenu
          at={menu.at}
          label={t("ws_model_title")}
          items={modelMenuLines({
            heading: t("ws_model_title"),
            models,
            protocol,
            model,
            effort,
            typed,
            focus: menu.focus,
            onModel: pickModel,
            onEffort: (e) => e !== effort && onPick(model, e),
          })}
          className="ws-model-pop"
          footer={
            <ModelField
              initial={custom ? model : ""}
              codex
              first={!models.length && !typed.length && !custom && menu.focus !== "effort"}
              onDone={(m) => {
                pickModel(m);
                setMenu(null);
              }}
            />
          }
          onClose={() => setMenu(null)}
        />
      )}
    </span>
  );
}
