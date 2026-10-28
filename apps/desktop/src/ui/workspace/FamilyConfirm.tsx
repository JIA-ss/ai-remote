// SB-37: archiving a session that has forks asks whether the forks go too — "Only this one" moves
// its forks to the top level, "All" archives them with it. Esc or a click outside cancels.

import { useRef, useSyncExternalStore } from "react";
import { t } from "../../lib/i18n";
import { isRunning } from "../../lib/workspace";
import { Modal } from "../Modal";
import { archiveFamily, familyAsk } from "./actions";
import { titleOf } from "./Sidebar";

export function FamilyConfirm() {
  const ask = useSyncExternalStore(familyAsk.subscribe, familyAsk.get);
  const first = useRef<HTMLButtonElement>(null);
  if (!ask) return null;
  const n = ask.forks.length;
  const working = ask.forks.filter((f) => isRunning(f.meta)).length;
  return (
    <Modal label={t("ws_family_archive_title")} role="alertdialog" describedBy="ws-family-b" className="ws-confirm"
      initialFocus={first} onClose={() => familyAsk.set(null)}>
        <h2 id="ws-family-h">{t("ws_family_archive_title")}</h2>
        <p id="ws-family-b">
          {t("ws_family_archive_body", { title: titleOf(ask.item), n })}
          {working > 0 && <> {t("ws_family_archive_working", { n: working })}</>}
        </p>
        <div className="ws-confirm-actions">
          <button type="button" className="ws-btn" onClick={() => familyAsk.set(null)}>
            {t("ws_family_cancel")}
          </button>
          <button type="button" className="ws-btn" ref={first} data-family-only onClick={() => archiveFamily(ask, false)}>
            {t("ws_family_archive_one")}
          </button>
          <button type="button" className="ws-btn ws-btn-primary" data-family-all onClick={() => archiveFamily(ask, true)}>
            {t("ws_family_archive_all", { n: n + 1 })}
          </button>
        </div>
    </Modal>
  );
}
