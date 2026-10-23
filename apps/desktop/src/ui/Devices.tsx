import { t } from "../lib/i18n";
import { groupOf, type Computer } from "../lib/store";
import type { Level } from "../lib/types";
import { OsGlyph } from "./glyphs";
import { Icon } from "./Icon";
import { statusOf } from "./Shell";
import { agentLabel, cx, routeLabel } from "./util";

const levelName = (level: Level | null) =>
  level ? t(level === "admin" ? "lvl_admin" : level === "control" ? "lvl_control" : "lvl_observe") : t("devices_no_access");

function accessText(c: Computer): string {
  if (c.feed.level !== undefined) {
    const level = levelName(c.feed.level);
    return c.online === true ? level : t("devices_last_access", { level });
  }
  return c.host.levelAtPairing
    ? t("devices_paired_access", { level: levelName(c.host.levelAtPairing) })
    : t("devices_access_unknown");
}

function connectionText(c: Computer): string {
  if (c.online === false) return t("st_offline");
  if (c.online === null) return t("st_connecting");
  return c.host.path === "direct" || c.host.path === "relay" ? routeLabel(c.host) : t("devices_path_unknown");
}

/** A workspace overview built only from paired hosts and their live feeds. Its title, count and
 *  "Pair a computer" live in the shell's toolbar; the sidebar's field (⌘F) searches it. */
export function DevicesPage({
  computers,
  now,
  query,
  onClearQuery,
  onPair,
  onApprovals,
  onDetails,
  onWatch,
}: {
  computers: Computer[];
  now: number;
  /** The sidebar's search. */
  query: string;
  onClearQuery: () => void;
  /** Only the empty state's button (the toolbar has the main one). */
  onPair: () => void;
  onApprovals: () => void;
  onDetails: (host: string) => void;
  onWatch: (host: string) => void;
}) {
  const online = computers.filter((c) => c.online === true);
  const pending = computers.reduce((total, c) => total + c.feed.pending.length, 0);
  const direct = online.filter((c) => c.host.path === "direct").length;
  const relay = online.filter((c) => c.host.path === "relay").length;
  const unknown = online.length - direct - relay;
  const rank = { needs: 0, working: 1, you: 2, idle: 3, offline: 4 };
  const matched = computers
    .filter((c) => c.host.name.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()))
    .sort((a, b) => rank[groupOf(a, now)] - rank[groupOf(b, now)] || a.host.name.localeCompare(b.host.name));

  return (
    <section className="devices-page" aria-label={t("devices")}>
      <div className="devices-summary">
        <div className="summary-item">
          <span><Icon name="monitor" size={16} />{t("devices_online")}</span>
          <strong>{online.length}<small> / {computers.length}</small></strong>
          <p>{t("devices_paired_count", { n: computers.length })}</p>
        </div>
        <button type="button" className={cx("summary-item", pending > 0 && "devices-attention")} onClick={onApprovals}>
          <span><Icon name="shield" size={16} />{t("approvals")}</span>
          <strong>{pending}</strong>
          <p>{t(pending > 0 ? "devices_approvals_waiting" : "devices_approvals_clear")}</p>
        </button>
        <div className="summary-item">
          <span><Icon name="lock" size={16} />{t("devices_encrypted")}</span>
          <strong>{online.length}</strong>
          <p>{t("devices_route_counts", { direct, relay, unknown })}</p>
        </div>
      </div>

      <div className="devices-toolbar">
        <h2>{t("devices_your_computers")} <span className="device-count">{matched.length}</span></h2>
      </div>

      {matched.length > 0 ? (
        <div className="device-grid">
          {matched.map((c) => {
            const status = statusOf(c, now);
            const group = groupOf(c, now);
            const agents = Object.values(c.feed.sessions).filter((session) => !session.person);
            const activeAgentNames = [...new Set(agents.map((session) => agentLabel(session.agent)))].join(t("sep"));
            const canWatch = c.online === true && c.feed.level !== null;
            return (
              <article className={cx("device-card", c.online === false && "off", c.feed.pending.length > 0 && "needs")} key={c.host.name}>
                <div className="device-card-top">
                  <span className="device-card-icon">{c.os ? <OsGlyph os={c.os} size={24} /> : <Icon name="monitor" size={24} />}</span>
                  <div className="device-card-name">
                    <h3>{c.host.name}</h3>
                    <span>{c.os ? t(c.os === "mac" ? "mac" : "win") : t("devices_os_unknown")}</span>
                  </div>
                  <span className={cx("device-card-status", c.online === true ? "online" : c.online === false ? "offline" : "connecting")}>
                    <i className="device-status-dot" />
                    {t(c.online === true ? "online_now" : c.online === false ? "st_offline" : "st_connecting")}
                  </span>
                </div>
                <p className={cx("device-card-agent", status.tone)} title={status.text}>{status.text}</p>
                <dl className="device-card-facts">
                  <div><dt>{t("your_level")}</dt><dd>{accessText(c)}</dd></div>
                  <div><dt>{t("connection")}</dt><dd>{connectionText(c)}</dd></div>
                  <div><dt>{t("agent_l")}</dt><dd>{c.online === true && agents.length > 0 ? activeAgentNames : t("devices_no_agent")}</dd></div>
                </dl>
                <div className="device-card-footer">
                  {c.feed.pending.length > 0 ? (
                    <button type="button" className="device-card-pending" onClick={onApprovals}>{t("devices_pending_count", { n: c.feed.pending.length })}<Icon name="chevR" size={12} /></button>
                  ) : <span>{t(group === "you" ? "st_you" : "devices_access_managed")}</span>}
                </div>
                <div className="device-card-actions">
                  <button type="button" className="btn primary" onClick={() => onWatch(c.host.name)} disabled={!canWatch} aria-label={t("devices_watch_host", { host: c.host.name })}>
                    <Icon name="eye" size={14} />{t("watch_live")}
                  </button>
                  <button type="button" className="btn" onClick={() => onDetails(c.host.name)} aria-label={t("devices_details_host", { host: c.host.name })}>
                    {t("devices_details")}<Icon name="chevR" size={14} />
                  </button>
                </div>
              </article>
            );
          })}
        </div>
      ) : (
        <div className="devices-empty" role="status">
          <Icon name={computers.length > 0 ? "search" : "monitor"} size={32} />
          <h2>{t(computers.length > 0 ? "search_no_matches" : "no_computers_short")}</h2>
          <p>{t(computers.length > 0 ? "devices_search_hint" : "devices_empty_hint")}</p>
          <button type="button" className="btn" onClick={computers.length > 0 ? onClearQuery : onPair}>{t(computers.length > 0 ? "clear_search" : "pair")}</button>
        </div>
      )}
      <p className="devices-connection-note"><Icon name="lock" size={12} />{t("devices_security_note")}</p>
    </section>
  );
}
