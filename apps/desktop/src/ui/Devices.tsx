import { t } from "../lib/i18n";
import { allPending, groupOf, type Computer } from "../lib/store";
import type { Level } from "../lib/types";
import { Glyph, OsGlyph, Pointer } from "./glyphs";
import { Icon } from "./Icon";
import { EmptyState } from "./pages/Empty";
import { statusOf } from "./Shell";
import { agentLabel, cx, routeLabel } from "./util";

const levelName = (level: Level | null) =>
  level ? t(level === "admin" ? "lvl_admin" : level === "control" ? "lvl_control" : "lvl_observe") : t("devices_no_access");
const levelGlyph = { observe: "level1", control: "level2", admin: "level3" } as const;

function accessText(c: Computer): string {
  if (c.feed.level !== undefined) {
    const level = levelName(c.feed.level);
    return c.online === true ? level : t("devices_last_access", { level });
  }
  return c.host.levelAtPairing
    ? t("devices_paired_access", { level: levelName(c.host.levelAtPairing) })
    : t("devices_access_unknown");
}

/** How it is reached, only while it is: offline and connecting are already the status pill. */
function connectionText(c: Computer): string | null {
  if (c.online !== true) return null;
  return c.host.path === "direct" || c.host.path === "relay" ? routeLabel(c.host) : t("devices_path_unknown");
}

/** Watching needs the computer online and some level there: the same rule as the computer page. */
export const canWatch = (c: Computer) => c.online === true && c.feed.level !== null;

/** A workspace overview built only from paired hosts and their live feeds. Its title, count and
 *  "Pair a computer" live in the shell's toolbar; the sidebar's field (⌘F) searches it. A strip on
 *  top appears only while requests wait; each card is the computer's name, status and at most two
 *  facts, with "Watch live" and "Details". */
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
  const pending = allPending({ ready: true, error: null, computers });
  const rank = { needs: 0, working: 1, you: 2, idle: 3, offline: 4 };
  const matched = computers
    .filter((c) => c.host.name.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()))
    .sort((a, b) => rank[groupOf(a, now)] - rank[groupOf(b, now)] || a.host.name.localeCompare(b.host.name));
  const first = pending[0];
  const firstComputer = first ? computers.find((c) => c.host.name === first.host) : undefined;

  return (
    <section className="devices-page" aria-label={t("devices")}>
      {first && (
        <button type="button" className="gl-attn" onClick={onApprovals}>
          <span className="gl-attn-ic" aria-hidden="true">
            <Pointer who="you" h={16} ui />
          </span>
          <span className="gl-attn-tx">
            <span className="gl-attn-l1">{pending.length === 1 ? t("inbox_one") : t("inbox_n", { n: pending.length })}</span>
            <span className="gl-attn-l2">{t("wants_tool", { agent: agentLabel(firstComputer?.feed.agent), tool: first.tool, host: first.host })}</span>
          </span>
          <span className="gl-attn-go">
            {t("devices_attention_go")}
            <Icon name="chevR" size={14} />
          </span>
        </button>
      )}

      {matched.length > 0 ? (
        <div className="device-grid">
          {matched.map((c) => {
            const status = statusOf(c, now);
            const watch = canWatch(c);
            const lvl = c.feed.level !== undefined ? c.feed.level : c.host.levelAtPairing;
            const conn = connectionText(c);
            const state = c.online === true ? "online" : c.online === false ? "offline" : "connecting";
            return (
              <article className={cx("device-card", c.online === false && "off", c.feed.pending.length > 0 && "needs")} key={c.host.name}>
                <div className="device-card-top">
                  <span className="device-card-icon">{c.os ? <OsGlyph os={c.os} size={20} /> : <Icon name="monitor" size={20} />}</span>
                  <div className="device-card-name">
                    <h3>{c.host.name}</h3>
                    <span>{c.os ? t(c.os === "mac" ? "mac" : "win") : t("devices_os_unknown")}</span>
                  </div>
                  <span className={cx("device-card-status", state)}>
                    <i className="device-status-dot" />
                    {t(c.online === true ? "online_now" : c.online === false ? "st_offline" : "st_connecting")}
                  </span>
                </div>
                <p className={cx("device-card-agent", status.tone)} title={status.text}>{status.text}</p>
                <p className="gl-dc-facts">
                  <span title={t("your_level")}>
                    <Glyph name={lvl ? levelGlyph[lvl] : "level1"} size={12} />
                    {accessText(c)}
                  </span>
                  {conn && (
                    <span title={t("connection")}>
                      <Glyph name={c.host.path === "relay" ? "relayed" : "direct"} size={12} />
                      {conn}
                    </span>
                  )}
                </p>
                <div className="device-card-actions">
                  <button type="button" className="btn gl-watch" onClick={() => onWatch(c.host.name)} disabled={!watch} aria-label={t("devices_watch_host", { host: c.host.name })}>
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
      ) : computers.length > 0 ? (
        <EmptyState
          className="devices-empty"
          icon="search"
          title={t("search_no_matches")}
          body={t("devices_search_hint")}
          action={<button type="button" className="btn" onClick={onClearQuery}>{t("clear_search")}</button>}
        />
      ) : (
        <EmptyState
          className="devices-empty"
          icon="monitor"
          title={t("no_computers_short")}
          body={t("devices_empty_hint")}
          action={<button type="button" className="btn primary" onClick={onPair}><Icon name="add" size={14} />{t("pair")}</button>}
        />
      )}
    </section>
  );
}
