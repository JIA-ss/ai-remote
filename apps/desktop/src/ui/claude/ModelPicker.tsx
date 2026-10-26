// The composer chin's model and effort button (CP-53, CP-57, NS-32, NS-33): the model's name and, after
// it in the secondary ink, the effort word; it opens the same menu as the new-session page, titled
// "Model · this session". A choice goes to the host as a `model` command and shows once the host says
// it holds (a `model` event): Claude Code switches models at once and restarts for another effort level
// once the turn is over (it resumes the conversation). ⌘⇧I / ⌘⇧E open it.

import { useState, type RefObject } from "react";
import type { ModelInfo } from "../../lib/chat";
import { isCodexRuntime } from "../../lib/codex";
import { t } from "../../lib/i18n";
import { keepEffort, modelChoices, rememberModel } from "../../lib/workspace";
import { cx } from "../util";
import { ModelChipText, modelChipClass, ModelField, modelKeys, modelMenuLines, useModelKeys, type ModelFocus } from "../workspace/ModelMenu";
import { PopMenu, type MenuAt } from "../workspace/parts";

/** Models typed for each agent (by runtime id), newest first: the new-session page's list. */
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

export function ModelPicker({ scope, runtime, protocol, models: offered, model, effort, disabled, onPick }: {
  scope: RefObject<HTMLElement | null>;
  runtime: string;
  protocol: string | undefined;
  models: readonly ModelInfo[];
  model: string;
  effort: string | null;
  disabled: boolean;
  onPick: (model: string, effort: string | null) => void;
}) {
  const [menu, setMenu] = useState<{ at: MenuAt; focus?: ModelFocus } | null>(null);
  const [btn, setBtn] = useState<HTMLButtonElement | null>(null);
  const models = modelChoices(offered, protocol);
  const open = (focus?: ModelFocus) => {
    if (btn) setMenu({ at: { anchor: btn.getBoundingClientRect(), side: "above" }, focus });
  };
  useModelKeys(scope, !disabled, open);
  const typed = typedFor(runtime);
  const pickModel = (m: string) => {
    if (m && !models.some((x) => x.id === m)) rememberTyped(runtime, m, protocol);
    // A level the new model does not take is not carried over (NS-33).
    if (m !== model) onPick(m, keepEffort(models, m, effort, protocol));
  };
  const lines = modelMenuLines({
    heading: t("ws_model_title"),
    models,
    protocol,
    model,
    effort,
    typed,
    focus: menu?.focus,
    onModel: pickModel,
    onEffort: (e) => e !== effort && onPick(model, e),
  });
  const custom = !!model && !models.some((m) => m.id === model);
  return (
    <span className="cl-model">
      <button
        ref={setBtn}
        type="button"
        className={cx(modelChipClass(model, effort), "cl-model-chip")}
        disabled={disabled}
        aria-haspopup="menu"
        aria-expanded={!!menu}
        title={t("ws_model_tip", { key: modelKeys })}
        onClick={(e) => {
          e.stopPropagation();
          if (menu) setMenu(null);
          else open();
        }}
      >
        <ModelChipText models={models} protocol={protocol} model={model} effort={effort} />
      </button>
      {menu && (
        <PopMenu
          at={menu.at}
          label={t("ws_model_title")}
          items={lines}
          className="ws-model-pop"
          footer={
            <ModelField
              initial={custom ? model : ""}
              codex={isCodexRuntime({ id: runtime, protocol })}
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
