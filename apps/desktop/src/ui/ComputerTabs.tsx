// The computer page's Access and Tools tabs, from the host's own answer (`{"access":true}`): this
// device's level and how to lower it, the standing rules that concern it, the devices paired there,
// unpairing; the tools on offer, built in and mounted, with the level each needs and which ask first.

import { useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import { currentLang, t, type Key } from "../lib/i18n";
import { store, type Computer } from "../lib/store";
import type { HostAccess, HostTool, Level } from "../lib/types";
import { type ChatTarget, StartForm, loginLabel, useRuntimes, useSessions } from "./AgentChat";
import { CmdLine } from "./CmdLine";
import { Glyph, Pointer } from "./glyphs";
import { Icon } from "./Icon";
import { Modal } from "./Modal";
import { agentLabel, cx, relayNote } from "./util";

const LEVELS: Level[] = ["observe", "control", "admin"];
const rank = (l: Level | null) => (l ? LEVELS.indexOf(l) : -1);
const glyph = { observe: "level1", control: "level2", admin: "level3" } as const;
const lvlName = (l: Level) => t(l === "admin" ? "lvl_admin" : l === "control" ? "lvl_control" : "lvl_observe");

/** The host's access answer, fetched when the tab opens; `reload` asks again. */
function useAccess(host: string) {
  const [state, setState] = useState<{ access: HostAccess | null; error: string | null }>({ access: null, error: null });
  const [round, setRound] = useState(0);
  useEffect(() => {
    let live = true;
    api.access(host).then(
      (access) => live && setState({ access, error: null }),
      (e) => live && setState({ access: null, error: String(e) }),
    );
    return () => {
      live = false;
    };
  }, [host, round]);
  return { ...state, reload: () => setRound((r) => r + 1) };
}

/** Headline and lede of a tab, or the reason the host's answer is missing. */
function TabHead({ hl, lede }: { hl: string; lede?: string | null }) {
  return (
    <>
      <div className="cp-hl tab">
        <div className="hd">{hl}</div>
      </div>
      {lede && <div className="cp-lede body">{lede}</div>}
    </>
  );
}

function Waiting({ host, error }: { host: string; error: string | null }) {
  return error ? (
    <TabHead hl={t("acc_err_hl", { host })} lede={error} />
  ) : (
    <TabHead hl={t("acc_loading", { host })} />
  );
}

/** A confirmation over the window: Cancel has the first focus, Esc cancels. */
export function Confirm({
  title,
  body,
  verb,
  danger,
  onYes,
  onNo,
}: {
  title: string;
  body: string;
  verb: string;
  danger?: boolean;
  onYes: () => Promise<void>;
  onNo: () => void;
}) {
  const cancel = useRef<HTMLButtonElement>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <Modal label={title} role="alertdialog" onClose={onNo} dismissible={!busy} initialFocus={cancel}>
        <div className="b">
          <div className="ttl">{title}</div>
          <div className="bd">{body}</div>
          {error && <div className="cap c-danger" style={{ marginTop: 10 }}>{error}</div>}
        </div>
        <div className="f">
          <button
            className={cx("btn", danger ? "danger" : "primary")}
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              try {
                await onYes();
              } catch (e) {
                setError(String(e));
                setBusy(false);
              }
            }}
          >
            {danger && <Glyph name="revoke" size={14} />}
            {verb}
          </button>
          <button className="btn" ref={cancel} disabled={busy} onClick={onNo}>
            {t("cancel")}
          </button>
        </div>
    </Modal>
  );
}

/** The computer itself: its system, how it is reached and its device key (the Access tab's first group). */
function ComputerFacts({ c }: { c: Computer }) {
  const conn = c.host.path ? t(c.host.path === "relay" ? (c.host.relayN0 ? "conn_relayed_n0" : "conn_relayed") : "conn_direct") : "—";
  const rows: Array<[string, React.ReactNode, string?]> = [
    [t("cp_os"), c.os ? t(c.os === "mac" ? "mac" : "win") : t("devices_os_unknown")],
    [t("connection"), conn, relayNote(c.host)?.detail],
    [t("device_key"), <span className="mono">{c.host.device ? c.host.device.slice(0, 16) : "—"}</span>],
  ];
  return (
    <div className="grp gl-facts">
      {rows.map(([label, value, title]) => (
        <div key={label} className="gr noic">
          <div className="tt">{label}</div>
          <span className="end gl-fact" title={title}>
            {value}
          </span>
        </div>
      ))}
    </div>
  );
}

export function AccessTab({ c, onUnpaired }: { c: Computer; onUnpaired: () => void }) {
  const host = c.host.name;
  const { access, error, reload } = useAccess(host);
  const [ask, setAsk] = useState<{ kind: "lower"; level: Level } | { kind: "unpair" } | null>(null);
  if (!access) return <Waiting host={host} error={error} />;
  const level = access.level;
  const admin = level === "admin";
  const desc = { observe: "lv_o_d", control: "lv_c_d", admin: "lv_a_d" } as const;
  const ruleText = { allow: "rule_allow", ask: "rule_ask", deny: "rule_deny" } as const;
  return (
    <>
      <TabHead
        hl={level ? t("acc_hl", { level: lvlName(level), host }) : t("acc_hl_none", { host })}
      />
      {access.bypass && (
        <div className="body c-danger" role="note" style={{ margin: "8px 0 0", maxWidth: 760, fontSize: 14, lineHeight: 1.65 }}>
          {t("acc_bypass", { host })}
        </div>
      )}
      <div className="tab-grid">
        <div>
          <div className="grp-h">{t("this_computer")}</div>
          <ComputerFacts c={c} />
          <div className="grp-h">{t("cp_acc_change")}</div>
          <div className="grp" role="radiogroup" aria-label={t("acc_level", { host })}>
            {LEVELS.map((l) => {
              const lower = rank(l) < rank(level);
              return (
                <div
                  key={l}
                  className={cx("gr", lower && "pick", rank(l) > rank(level) && "above")}
                  role="radio"
                  aria-checked={l === level}
                  aria-disabled={!lower && l !== level}
                  tabIndex={lower ? 0 : -1}
                  onClick={() => lower && setAsk({ kind: "lower", level: l })}
                  onKeyDown={(e) => lower && (e.key === "Enter" || e.key === " ") && setAsk({ kind: "lower", level: l })}
                >
                  <span className="ic bare">
                    <span className={cx("radio", l === level && "on")} />
                  </span>
                  <div>
                    <div className="tt lvlname">
                      <Glyph name={glyph[l]} size={14} />
                      {lvlName(l)}
                    </div>
                    <div className="ds">{t(desc[l])}</div>
                  </div>
                  <span className="end" />
                </div>
              );
            })}
          </div>
          <div className="gnote">{t("acc_level_note", { host })}</div>

          <div className="grp-h" style={{ marginTop: 28 }}>
            {t("acc_standing")}
          </div>
          <div className="grp-sub">{t("acc_standing_sub", { host })}</div>
          {access.rules.length === 0 ? (
            <div className="gnote" style={{ marginTop: 0 }}>
              {t("rules_none")}
            </div>
          ) : (
            <div className="grp">
              {access.rules.map((r, i) => (
                <div key={i} className="gr">
                  <span className="ic">
                    <Glyph name={r.action === "deny" ? "revoke" : "approval"} size={16} />
                  </span>
                  <div>
                    <div className="tt">{t(ruleText[r.action], { tool: r.tool })}</div>
                    <div className="ds">{r.scope === "you" ? t("rule_you") : t("rule_all", { host })}</div>
                  </div>
                  <span className="end" />
                </div>
              ))}
            </div>
          )}
        </div>

        <div>
          <div className="grp-h">{t("acc_devices", { host })}</div>
          <div className="grp">
            {access.devices.map((d) => (
              <div key={d.name} className="gr">
                <span className="ic">
                  <Icon name={d.you ? "laptop" : "phone"} />
                </span>
                <div>
                  <div className="tt">{d.name}</div>
                  <div className="ds">{d.revoked ? t("dev_revoked") : d.you ? t("dev_this_d") : t("dev_other_d", { host })}</div>
                </div>
                <span className="end">
                  {d.level && (
                    <span className="lvlword">
                      <Glyph name={glyph[d.level]} size={12} />
                      {lvlName(d.level)}
                    </span>
                  )}
                </span>
              </div>
            ))}
          </div>
          {!admin && !access.bypass && <div className="gnote">{t("acc_devices_note")}</div>}
          <div className="grp" style={{ marginTop: 20 }}>
            <div className="gr noic" style={{ alignItems: "start" }}>
              <div>
                <div className="tt">{t("unpair", { host })}</div>
                <div className="ds">{t("unpair_d", { host })}</div>
                <div style={{ marginTop: 12 }}>
                  <button className="btn" style={{ color: "var(--danger)" }} onClick={() => setAsk({ kind: "unpair" })}>
                    <Glyph name="revoke" size={14} />
                    {t("unpair", { host })}
                  </button>
                </div>
              </div>
              <span />
            </div>
          </div>
        </div>
      </div>
      {ask?.kind === "lower" && (
        <Confirm
          title={t("lower_q", { level: lvlName(ask.level) })}
          body={t("lower_body", { level: lvlName(ask.level), host })}
          verb={t("lower_btn", { level: lvlName(ask.level) })}
          onNo={() => setAsk(null)}
          onYes={async () => {
            await api.lowerLevel(host, ask.level);
            setAsk(null);
            reload();
            await store.refresh();
          }}
        />
      )}
      {ask?.kind === "unpair" && (
        <Confirm
          title={t("unpair_q", { host })}
          body={t("unpair_body", { host })}
          verb={t("unpair_btn")}
          danger
          onNo={() => setAsk(null)}
          onYes={async () => {
            await api.unpair(host);
            await store.refresh();
            onUnpaired();
          }}
        />
      )}
    </>
  );
}

/** The first sentence of a tool's description (the rest is written for the agent). */
const gist = (d: string) => d.split(/(?<=\.)\s/)[0];

export function ToolsTab({ c }: { c: Computer }) {
  const host = c.host.name;
  const { access, error } = useAccess(host);
  const [sel, setSel] = useState<string | null>(null);
  const [how, setHow] = useState(false);
  if (!access) return <Waiting host={host} error={error} />;
  const level = access.level;
  const sources = [
    { key: "", name: "ai-remote", builtin: true, tools: access.builtin, ds: t("src_builtin") },
    ...access.mounts.map((m) => ({
      key: m.name,
      name: m.name,
      builtin: false,
      tools: m.tools,
      ds: [m.lazy ? t("src_lazy") : t("src_eager"), m.needs_desktop ? t("src_desktop") : null].filter(Boolean).join(t("sep")),
    })),
  ];
  const all = sources.flatMap((s) => s.tools);
  const usable = (x: HostTool) => rank(x.level) <= rank(level) && x.action !== "deny";
  const n = all.filter(usable).length;
  const asks = all.filter((x) => usable(x) && x.action === "ask").length;
  const above = all.filter((x) => rank(x.level) > rank(level)).length;
  const mounted = access.mounts.reduce((k, m) => k + m.tools.filter(usable).length, 0);
  const current = sources.find((s) => s.key === (sel ?? "")) ?? sources[0];
  const maxLevel = (tools: HostTool[]) => tools.reduce<Level>((m, x) => (rank(x.level) > rank(m) ? x.level : m), "observe");
  const lede = [
    access.mounts.length ? t("tools_lede", { b: n - mounted, m: mounted }) : t("tools_lede_builtin", { b: n }),
    asks ? t("tools_asks", { k: asks }) : null,
    above ? t("tools_above", { x: above }) : null,
  ]
    .filter(Boolean)
    .join(currentLang() === "zh" ? "" : " ");
  return (
    <>
      <TabHead hl={t("tools_hl", { agent: agentLabel(c.feed.agent), n, host })} lede={lede} />
      <div className="tab-grid">
        <div>
          <div className="grp-h">
            {t("tool_sources")}
            <span className="end">
              <button className="btn sm" style={{ height: 28, fontSize: 12 }} onClick={() => setHow((v) => !v)} aria-expanded={how}>
                <Icon name="add" size={12} />
                {t("add_server")}
              </button>
            </span>
          </div>
          {how && (
            <div className="grp" style={{ marginBottom: 12 }}>
              <div className="gr noic">
                <div>
                  <div className="ds" style={{ marginTop: 0 }}>
                    {t("add_server_how", { host })}
                  </div>
                  <CmdLine cmd="ai-remote mount add NAME --command PATH --tool TOOL=control" style={{ marginTop: 8 }} />
                </div>
                <span />
              </div>
            </div>
          )}
          <div className="grp">
            {sources.map((s) => (
              <div
                key={s.key || "builtin"}
                className={cx("gr", "pick", s === current && "sel")}
                role="button"
                tabIndex={0}
                aria-pressed={s === current}
                onClick={() => setSel(s.key)}
                onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && setSel(s.key)}
              >
                <span className="ic">
                  <Icon name={s.builtin ? "wrench" : "puzzle"} />
                </span>
                <div>
                  <div className="tt">
                    <span className="mono" style={{ fontSize: 14 }}>
                      {s.name}
                    </span>
                    <span className="meta" style={{ marginLeft: 10 }}>
                      {t("n_tools", { n: s.tools.length })}
                    </span>
                  </div>
                  <div className="ds">{s.ds}</div>
                </div>
                <span className="end">
                  {s.tools.length > 0 && (
                    <span className="lvlword">
                      <Glyph name={glyph[maxLevel(s.tools)]} size={12} />
                      {t("needs", { l: lvlName(maxLevel(s.tools)) })}
                    </span>
                  )}
                  <span style={{ color: "var(--text-3)" }}>
                    <Icon name="chevR" size={12} />
                  </span>
                </span>
              </div>
            ))}
          </div>
          <div className="gnote">{access.mounts.length ? t("tools_sub", { host }) : t("tools_sub_none", { host })}</div>
        </div>

        <div>
          <div className="grp-h">
            <span className="mono" style={{ fontSize: 12, color: "var(--text-1)", fontWeight: 400 }}>
              {current.name}
            </span>
            &nbsp;&nbsp;{t("n_tools", { n: current.tools.length })}
          </div>
          <div className="grp">
            {current.tools.map((x) => (
              <div key={x.name} className={cx("gr noic tool", rank(x.level) > rank(level) && "above")}>
                <div className="tool-l">
                  <span className="mono">{x.name}</span>
                  <span className="c2 ell">{gist(x.description)}</span>
                </div>
                <span className="end">
                  {rank(x.level) > rank(level) ? (
                    <span className="lvlword">
                      <Glyph name={glyph[x.level]} size={12} />
                      {t("needs", { l: lvlName(x.level) })}
                    </span>
                  ) : x.action === "deny" ? (
                    <span className="cap c3">{t("tool_refused")}</span>
                  ) : x.action === "ask" ? (
                    <span className="cap c-you" style={{ display: "inline-flex", gap: 6, alignItems: "center" }}>
                      <Pointer who="you" h={10} ui />
                      {t("asks_first")}
                    </span>
                  ) : null}
                </span>
              </div>
            ))}
          </div>
          <div className="gnote">{t("t_note")}</div>
        </div>
      </div>
    </>
  );
}

/**
 * The coding agents installed on the computer (Claude Code, Codex, ...): version, sign-in, where;
 * the sessions running there; and starting one, which opens its chat beside the live view.
 */
export function AgentsTab({ c, onOpenChat }: { c: Computer; onOpenChat: (target: ChatTarget) => void }) {
  const host = c.host.name;
  const { report, error, refresh } = useRuntimes(host);
  const [tick, setTick] = useState(0);
  const sessions = useSessions(host, tick);
  if (error) return <TabHead hl={t("hc_hl_err", { host })} lede={error} />;
  if (!report) return <TabHead hl={t("hc_looking", { host })} />;
  const installed = report.runtimes.filter((r) => r.verdict !== "missing");
  const missing = report.runtimes.filter((r) => r.verdict === "missing");
  const open = (sessions ?? []).filter((s) => s.state !== "ended");
  const end = async (id: string) => {
    await api.callTool(host, "chat_end", { session: id }, true).catch(() => {});
    setTimeout(() => setTick((n) => n + 1), 400);
  };
  return (
    <>
      <TabHead
        hl={installed.length ? t("hc_hl", { n: installed.length, host }) : t("hc_hl_none", { host })}
        lede={report.host.can_start ? (report.host.enabled === false ? t("hc_disabled", { host }) : t("hc_lede")) : report.host.reason}
      />
      <div className="tab-grid">
        <div>
          <div className="grp-h">
            {t("hc_installed")}
            <span className="end">
              <button className="btn sm" style={{ height: 28, fontSize: 12 }} onClick={refresh}>
                <Icon name="refresh" size={12} />
                {t("hc_refresh")}
              </button>
            </span>
          </div>
          <div className="grp">
            {installed.map((r) => (
              <div key={r.id} className="gr noic tool">
                <div className="tool-l" style={{ flexDirection: "column", alignItems: "flex-start", gap: 2 }}>
                  <span style={{ fontSize: 14 }}>
                    {r.name}
                    <span className="mono meta" style={{ marginLeft: 8 }}>
                      {r.version ?? ""}
                    </span>
                  </span>
                  <span className="c2 ell mono" style={{ fontSize: 11, maxWidth: 420 }} title={r.path}>
                    {r.path}
                  </span>
                  {r.detail && <span className="c3" style={{ fontSize: 12 }}>{r.detail}</span>}
                </div>
                <span className="end">
                  {r.verdict === "ok" ? (
                    <span className={cx("cap", r.login?.state === "logged_out" ? "c-danger" : "c2")}>{loginLabel(r) ?? t("hc_ready")}</span>
                  ) : (
                    <span className="cap c-danger">{r.verdict === "below_minimum" ? t("hc_too_old") : t("hc_broken")}</span>
                  )}
                </span>
              </div>
            ))}
            {installed.length === 0 && (
              <div className="gr noic">
                <div className="ds" style={{ marginTop: 0 }}>
                  {t("hc_none", { host })}
                </div>
                <span />
              </div>
            )}
          </div>
          {missing.length > 0 && <div className="gnote">{t("hc_missing", { list: missing.map((r) => r.name).join(t("sep")) })}</div>}
          {open.length > 0 && (
            <>
              <div className="grp-h" style={{ marginTop: 24 }}>
                {t("hc_sessions")}
              </div>
              <div className="grp">
                {open.map((s) => (
                  <div key={s.id} className="gr noic tool">
                    <div className="tool-l" style={{ flexDirection: "column", alignItems: "flex-start", gap: 2 }}>
                      <span style={{ fontSize: 14 }}>
                        {s.name}
                        <span className="meta" style={{ marginLeft: 8 }}>
                          {t(`hc_state_${s.state}` as Key)} · {t("hc_turns", { n: s.turns })}
                        </span>
                      </span>
                      <span className="c2 ell mono" style={{ fontSize: 11, maxWidth: 420 }}>
                        {s.cwd}
                      </span>
                    </div>
                    <span className="end" style={{ gap: 8, display: "inline-flex" }}>
                      <button className="btn sm" onClick={() => onOpenChat({ session: s.id })}>
                        {t("hc_open")}
                      </button>
                      <button className="btn sm" onClick={() => void end(s.id)}>
                        {t("hc_end")}
                      </button>
                    </span>
                  </div>
                ))}
              </div>
            </>
          )}
        </div>
        <div>
          <div className="grp-h">{t("hc_new")}</div>
          <div className="grp" style={{ padding: 16 }}>
            <StartForm host={host} report={report} onStarted={onOpenChat} />
          </div>
          <div className="gnote">{t("hc_note")}</div>
        </div>
      </div>
    </>
  );
}
