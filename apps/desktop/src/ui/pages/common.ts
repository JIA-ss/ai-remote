// Small helpers the main-window pages share: an access level's name and what a resolved approval says.

import { t } from "../../lib/i18n";
import type { Step } from "../../lib/types";

export const levelName = (l: string | null) =>
  l === "admin" ? t("lvl_admin") : l === "control" ? t("lvl_control") : l === "observe" ? t("lvl_observe") : "—";

/** What a resolved approval's receipt says. */
export function receiptOf(s: Step, host: string): { q: string; a: string; you: boolean } {
  const q = t("s_asked", { tool: s.tool });
  if (s.decision === "timeout" || s.decision === "unavailable") return { q, a: t("s_timeout"), you: false };
  const allowed = s.decision === "approved";
  if (s.answeredBy === "host") return { q, a: t(allowed ? "s_allowed_host" : "s_denied_host", { host }), you: false };
  return { q, a: t(allowed ? "s_allowed" : "s_denied", { device: s.answeredBy ?? "" }), you: true };
}
