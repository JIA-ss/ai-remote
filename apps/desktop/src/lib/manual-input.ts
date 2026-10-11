export interface ManualInputAction {
  tool: string;
  args: Record<string, unknown>;
}

export interface ManualInputFailure {
  reason: "overflow" | "tool";
  message: string;
}

export interface ManualInputDispatcher {
  enqueue(tool: string, args: Record<string, unknown>): boolean;
  /** Drops unsent actions; an already sent action is never replayed. */
  cancel(): void;
}

const MAX_QUEUED_ACTIONS = 32;
const MAX_COALESCED_TEXT = 1024;

function plainText(action: ManualInputAction): string | null {
  return action.tool === "keyboard" && Object.keys(action.args).length === 1 && typeof action.args.text === "string"
    ? action.args.text
    : null;
}

function toolFailure(result: unknown): string | null {
  if (!result || typeof result !== "object" || !("isError" in result) || result.isError !== true) return null;
  const content = "content" in result && Array.isArray(result.content) ? result.content : [];
  return content
    .filter((part): part is { type: string; text: string } => !!part && typeof part === "object" && part.type === "text" && typeof part.text === "string")
    .map((part) => part.text)
    .join("\n") || "The host refused the input.";
}

/** Serialises manual calls so rapid input cannot overrun the host's bounded input gate. */
export function createManualInputDispatcher(
  send: (tool: string, args: Record<string, unknown>) => Promise<unknown>,
  onError: (failure: ManualInputFailure) => void,
): ManualInputDispatcher {
  const queue: ManualInputAction[] = [];
  let inFlight = false;
  let generation = 0;

  const fail = (message: string) => {
    queue.length = 0;
    onError({ reason: "tool", message });
  };

  const pump = async () => {
    if (inFlight || queue.length === 0) return;
    const action = queue.shift()!;
    const sentGeneration = generation;
    inFlight = true;
    try {
      const result = await send(action.tool, action.args);
      const error = toolFailure(result);
      if (error && sentGeneration === generation) fail(error);
    } catch (error) {
      if (sentGeneration === generation) fail(error instanceof Error ? error.message : String(error));
    } finally {
      inFlight = false;
      void pump();
    }
  };

  return {
    enqueue(tool, args) {
      const action = { tool, args: { ...args } };
      const last = queue.at(-1);
      const addedText = plainText(action);
      const previousText = last && plainText(last);
      if (last && addedText !== null && previousText !== null && previousText !== undefined && previousText.length + addedText.length <= MAX_COALESCED_TEXT) {
        last.args.text = previousText + addedText;
      } else {
        if (queue.length >= MAX_QUEUED_ACTIONS) {
          queue.length = 0;
          onError({ reason: "overflow", message: "Too many remote input actions are waiting." });
          return false;
        }
        queue.push(action);
      }
      void pump();
      return true;
    },
    cancel() {
      generation++;
      queue.length = 0;
    },
  };
}
