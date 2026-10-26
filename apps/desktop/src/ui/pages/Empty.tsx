// The one empty-state shape every main page uses (no computers, no search match, nothing to
// approve, no activity, a computer that is gone): centred, no card, an icon, a title, one line and
// at most one button. MissingComputer is the page for a route whose computer the list no longer has.

import type { ReactNode } from "react";
import { t } from "../../lib/i18n";
import { Icon, type IconName } from "../Icon";
import { cx } from "../util";

export function EmptyState({
  icon,
  title,
  body,
  action,
  className,
  compact,
}: {
  icon: IconName;
  title: string;
  body?: string;
  /** One button, or nothing. */
  action?: ReactNode;
  /** Extra classes on the root (tests look for `.devices-empty`). */
  className?: string;
  /** Not the whole page: a block among other content (approvals with today's receipts). */
  compact?: boolean;
}) {
  return (
    <div className={cx("gl-blank", compact && "compact", className)} role="status">
      <span className="gl-blank-ic" aria-hidden="true">
        <Icon name={icon} size={32} />
      </span>
      <div className="gl-blank-ttl">{title}</div>
      {body && <div className="gl-blank-body">{body}</div>}
      {action && <div className="gl-blank-act">{action}</div>}
    </div>
  );
}

/** A computer the route names but the list no longer has (unpaired elsewhere, or a stale deep link). */
export function MissingComputer({ host, onDevices }: { host: string; onDevices: () => void }) {
  return (
    <div className="ipage gl-blankpage">
      <EmptyState
        icon="monitor"
        title={t("missing_title", { host })}
        body={t("missing_body")}
        action={
          <button type="button" className="btn" onClick={onDevices}>
            {t("back_devices")}
          </button>
        }
      />
    </div>
  );
}
