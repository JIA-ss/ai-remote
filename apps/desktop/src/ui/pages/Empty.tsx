// A computer the route names but the list no longer has (unpaired elsewhere, or a stale deep link).

import { t } from "../../lib/i18n";
import { Icon } from "../Icon";

export function MissingComputer({ host, onDevices }: { host: string; onDevices: () => void }) {
  return (
    <div className="ipage gl-empty" role="status">
      <Icon name="monitor" size={32} />
      <div className="ttl">{t("missing_title", { host })}</div>
      <div className="body c2">{t("missing_body")}</div>
      <div>
        <button type="button" className="btn" onClick={onDevices}>
          {t("back_devices")}
        </button>
      </div>
    </div>
  );
}
