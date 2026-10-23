// A Codex session's conversation: the chat panel's header, find, quoting, composer and previews, with
// its turns laid out the way Codex's own app shows them (CodexTurn.tsx, lib/codex.ts). The panel picks
// this layout by itself from the agent the host names (`runtime.protocol`); this is the entry for
// views that dispatch on the agent themselves.

import { Conversation, type ChatTarget } from "../AgentChat";

export { CodexTurnView } from "./CodexTurn";
export { isCodexRuntime } from "../../lib/codex";

export function CodexConversation(props: { host: string; target: ChatTarget; onLeave: (gone?: boolean) => void }) {
  return <Conversation {...props} />;
}
