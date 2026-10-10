// Settings, a window of its own (back returns to the main window): General, Agents, This device,
// Approvals and About. Only what works today is shown; nothing here is a switch that does nothing.

import { useEffect, useState } from "react";
import { api } from "../lib/api";
import { followSystemLang, langChoice, setLang, t, type Key } from "../lib/i18n";
import { detectOs } from "../lib/platform";
import { MCP_CLIENTS, mcpInstall, type McpClient } from "../lib/mcp-install";
import { useStore } from "../lib/store";
import type { AgentStatus } from "../lib/types";
import { CmdLine } from "./CmdLine";
import { Glyph, Mark, Pointer } from "./glyphs";
import { Icon, type IconName } from "./Icon";
import { TitleBar } from "./Shell";
import { clock, cx } from "./util";

type Section = "general" | "agents" | "device" | "approvals" | "about";

const SECTIONS: Array<[Section, IconName | "approval", Key]> = [
  ["general", "settings", "set_general"],
  ["agents", "terminal", "set_agents"],
  ["device", "laptop", "set_device"],
  ["approvals", "approval", "set_approvals"],
  ["about", "info", "set_about"],
];

function initialSection(): Section {
  const v = new URLSearchParams(location.search).get("sec");
  return SECTIONS.some(([s]) => s === v) ? (v as Section) : "general";
}

const mac = detectOs() === "mac";

/** One settings row: icon, title and description, and its control at the end. */
function Row({ ic, tt, ds, children }: { ic: React.ReactNode; tt: React.ReactNode; ds?: React.ReactNode; children?: React.ReactNode }) {
  return (
    <div className="gr">
      <span className="ic">{ic}</span>
      <div>
        <div className="tt">{tt}</div>
        {ds && <div className="ds">{ds}</div>}
      </div>
      <span className="end">{children}</span>
    </div>
  );
}

export function SettingsWindow({ onBack }: { onBack: () => void }) {
  const [sec, setSec] = useState<Section>(initialSection);
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [round, setRound] = useState(0);
  useEffect(() => {
    api.status().then(setStatus, () => setStatus(null));
  }, [round]);
  const name = status?.name ?? "";
  const heads: Record<Section, [Key, Key]> = {
    general: ["set_general", mac ? "gn_lede_mac" : "gn_lede"],
    agents: ["set_agents", "ag_lede"],
    device: ["set_device", "dv_lede"],
    approvals: ["set_approvals", "ap_lede"],
    about: ["set_about", "ab_lede"],
  };
  const [hd, lede] = heads[sec];
  return (
    <div className="win swin" style={{ width: "100vw", height: "100vh" }}>
      <TitleBar title={t("settings")} onBack={onBack} />
      <div className="body-row">
        <nav className="snav">
          <div className="me">
            <span className="tile">
              <Icon name="laptop" size={20} />
            </span>
            <div style={{ display: "flex", flexDirection: "column", minWidth: 0 }}>
              <span className="strong ell">{name || "—"}</span>
              <span className="cap c3">{t(mac ? "mac" : "win")}</span>
            </div>
          </div>
          {SECTIONS.map(([s, ic, label]) => (
            <div
              key={s}
              className={cx("nv", s === sec && "sel")}
              role="button"
              tabIndex={0}
              aria-current={s === sec}
              onClick={() => setSec(s)}
              onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && setSec(s)}
            >
              {ic === "approval" ? <Glyph name="approval" size={16} /> : <Icon name={ic} size={16} />}
              <span>{t(label)}</span>
            </div>
          ))}
        </nav>
        <main className="page">
          <div className="spage" style={{ overflowY: "auto" }}>
            <div className="hd">{t(hd)}</div>
            <div className="body c2" style={{ marginTop: 6, maxWidth: 640 }}>
              {t(lede, { name })}
            </div>
            <div style={{ marginTop: 28 }}>
              {sec === "general" && <General version={status?.version} />}
              {sec === "agents" && <Agents status={status} name={name} onChange={() => setRound((r) => r + 1)} />}
              {sec === "device" && <Device status={status} />}
              {sec === "approvals" && <Approvals />}
              {sec === "about" && <About version={status?.version} />}
            </div>
          </div>
        </main>
      </div>
    </div>
  );
}

function General({ version }: { version?: string }) {
  const [choice, setChoice] = useState(langChoice());
  return (
    <div className="grp">
      <Row ic={<Icon name="globe" />} tt={t("gn_lang")} ds={t(mac ? "gn_lang_d_mac" : "gn_lang_d")}>
        <span className="dd-wrap">
          <select
            className="dd"
            aria-label={t("gn_lang")}
          value={choice}
          onChange={(e) => {
            const v = e.target.value as "system" | "en" | "zh";
            setChoice(v);
            if (v === "system") followSystemLang();
            else setLang(v);
            location.reload();
          }}
        >
            <option value="system">{t(mac ? "gn_lang_v_mac" : "gn_lang_v")}</option>
            <option value="en">English</option>
            <option value="zh">中文</option>
          </select>
          <Icon name="chevD" size={12} />
        </span>
      </Row>
      <Row ic={<Icon name="monitor" />} tt={t("gn_theme")} ds={t(mac ? "gn_theme_d_mac" : "gn_theme_d")}>
        <button className="btn" onClick={() => void api.openAppearanceSettings().catch(() => {})}>
          <Icon name="open" size={14} />
          {t(mac ? "gn_open_mac" : "gn_open_win")}
        </button>
      </Row>
      <Row ic={<Icon name="keyboard" />} tt={t("gn_hotkey")} ds={t("gn_hotkey_d")}>
        <span className="kbd" style={{ fontSize: 13, lineHeight: "22px", padding: "0 8px", color: "var(--text-1)" }}>
          {mac ? "⌃⌥H" : "Ctrl+Alt+H"}
        </span>
      </Row>
      <Row ic={<Mark h={14} />} tt={t("gn_version")} ds={t("gn_version_d")}>
        <span className="mono" style={{ fontSize: 13 }}>
          {version ?? "—"}
        </span>
      </Row>
    </div>
  );
}

function Agents({ status, name, onChange }: { status: AgentStatus | null; name: string; onChange: () => void }) {
  const agents = status?.agents ?? [];
  const [client, setClient] = useState<McpClient>(() => {
    const saved = localStorage.getItem("ai-remote.mcp-client");
    return MCP_CLIENTS.some(([id]) => id === saved) ? saved as McpClient : "claude";
  });
  const setup = status?.mcp ? mcpInstall(client, status.mcp, !mac) : null;
  const instructions: Record<McpClient, Key> = {
    claude: "ag_claude_d", codex: "ag_codex_d", cursor: "ag_cursor_d", vscode: "ag_vscode_d", other: "ag_other_d",
  };
  return (
    <>
      <div className="grp-h">{t("ag_connected")}</div>
      {agents.length > 0 && (
        <div className="grp">
          {agents.map((a) => (
            <Row
              key={a.id}
              ic={
                <span style={{ display: "inline-flex" }}>
                  <Pointer who="agent" h={15} ui />
                </span>
              }
              tt={
                <>
                  {a.client ?? t("ag_unnamed")}
                  <span className="facts" style={{ display: "inline-flex", marginLeft: 10, verticalAlign: 1 }}>
                    <span>
                      <i className="on" />
                      {t("online_now")}
                    </span>
                  </span>
                </>
              }
              ds={t(a.calls ? "ag_seen" : "ag_seen_none", { name, time: clock(a.last_ms, false), n: a.calls })}
            >
              <button
                className="btn"
                onClick={async () => {
                  await api.disconnectAgent(a.id).catch(() => {});
                  onChange();
                }}
              >
                {t("disconnect")}
              </button>
            </Row>
          ))}
        </div>
      )}
      <div className="gnote" style={agents.length ? undefined : { marginTop: 0 }}>
        {t(agents.length ? "ag_none_other" : "ag_none")}
      </div>
      <div className="grp-h" style={{ marginTop: 28 }}>
        {t("ag_add")}
      </div>
      <div className="grp">
        <div className="gr noic" style={{ display: "block", padding: 16 }}>
          <div className="ds" style={{ fontSize: 14, lineHeight: "var(--lh-body)", margin: "0 0 12px" }}>
            {t("ag_add_d", { name })}
          </div>
          <label style={{ display: "flex", alignItems: "center", gap: 12, marginBottom: 16 }}>
            <span className="ds">{t("ag_client")}</span>
            <select className="dd" value={client} onChange={(e) => {
              const next = e.target.value as McpClient;
              setClient(next);
              localStorage.setItem("ai-remote.mcp-client", next);
            }}>
              {MCP_CLIENTS.map(([id, label]) => <option key={id} value={id}>{label || t("ag_other")}</option>)}
            </select>
          </label>
          <div className="ds" style={{ marginBottom: 12 }}>{t(instructions[client])}</div>
          {setup ? <>
            {setup.command && <>
              <div className="ds" style={{ marginBottom: 8 }}>{t(mac ? "ag_terminal_mac" : "ag_terminal_win")}</div>
              <CmdLine key={setup.command} cmd={setup.command} multiline />
            </>}
            {setup.config && <>
              {client === "codex" && <div className="ds" style={{ margin: "16px 0 8px" }}>{t("ag_codex_manual")}</div>}
              <CmdLine key={setup.config} cmd={setup.config} multiline />
              <div className="ds" style={{ marginTop: 10 }}>{t("ag_merge")}</div>
            </>}
            <div className="ds" style={{ marginTop: 14 }}>{t("ag_verify")}</div>
          </> : <div className="ds">{t("ag_config_missing")}</div>}
        </div>
      </div>
    </>
  );
}

/** `eid:4f2a9c1e…` as `eid:4f2a 9c1e 77b0 d3a8`: enough to compare by eye. */
const shortKey = (id: string) => {
  const hex = id.replace(/^eid:/, "");
  return `eid:${hex.slice(0, 16).replace(/(.{4})(?=.)/g, "$1 ")}`;
};

function Device({ status }: { status: AgentStatus | null }) {
  const [copied, setCopied] = useState(false);
  const id = status?.device ?? "";
  return (
    <>
      <div className="grp">
        <Row ic={<Icon name="laptop" />} tt={t("dv_name")} ds={t("dv_name_d")}>
          <span className="body">{status?.name ?? "—"}</span>
        </Row>
        <Row ic={<Icon name="key" />} tt={t("dv_key")} ds={t("dv_key_d")}>
          <span className="mono" style={{ fontSize: 13 }}>
            {id ? shortKey(id) : "—"}
          </span>
          <button
            className="btn icon"
            aria-label={t("copy")}
            disabled={!id}
            onClick={() => {
              void navigator.clipboard?.writeText(id);
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            }}
          >
            <Icon name={copied ? "check" : "copy"} size={14} />
          </button>
        </Row>
      </div>
      <div className="grp-h" style={{ marginTop: 28 }}>
        {t("dv_host")}
      </div>
      <div className="grp">
        <div className="gr noic" style={{ display: "block", padding: 16 }}>
          <div className="ds" style={{ fontSize: 14, lineHeight: "var(--lh-body)", margin: "0 0 12px" }}>
            {t("dv_host_d")}
          </div>
          <CmdLine cmd="ai-remote host install" />
          <div className="ds" style={{ marginTop: 10 }}>
            {t("dv_host_note")}
          </div>
        </div>
      </div>
    </>
  );
}

function Approvals() {
  const snap = useStore();
  const hosts = snap.computers.map((c) => c.host.name);
  const [state, setState] = useState<Record<string, boolean | null | "error" | "asking">>({});
  const [round, setRound] = useState(0);
  useEffect(() => {
    let live = true;
    for (const h of hosts) {
      api.approverStatus(h).then(
        (v) => live && setState((s) => ({ ...s, [h]: v })),
        () => live && setState((s) => ({ ...s, [h]: "error" })),
      );
    }
    return () => {
      live = false;
    };
    // The list of names, not the snapshot object, decides when to ask again.
  }, [hosts.join("\n"), round]);
  return (
    <>
      <div className="grp">
        <Row ic={<Icon name="shield" />} tt={t(mac ? "ap_key_mac" : "ap_key")} ds={t(mac ? "ap_key_d_mac" : "ap_key_d")} />
        {hosts.map((h) => {
          const v = state[h];
          return (
            <div key={h} className="gr" style={{ minHeight: 48, paddingLeft: 62 }}>
              <span className="ic bare">
                <span className={cx("check", v === true && "on")}>{v === true && <Icon name="check" size={12} />}</span>
              </span>
              <div>
                <div className="tt">{h}</div>
                <div className="ds">
                  {mac ? t("ap_mac_deny_only") : v === true
                    ? t("ap_accepted")
                    : v === "asking"
                      ? t("ap_asking", { host: h })
                      : v === "error"
                        ? t("ap_unreachable")
                        : v === undefined
                          ? t("acc_loading", { host: h })
                          : t("ap_not_yet")}
                </div>
              </div>
              <span className="end">
                {!mac && (v === false || v === null) && (
                  <button
                    className="btn"
                    onClick={async () => {
                      setState((s) => ({ ...s, [h]: "asking" }));
                      await api.enrollApprover(h).catch(() => {});
                      setRound((r) => r + 1);
                    }}
                  >
                    {t("ap_offer")}
                  </button>
                )}
              </span>
            </div>
          );
        })}
        <Row ic={<Icon name="clock" />} tt={t("ap_timeout")} ds={t("ap_timeout_d2")} />
      </div>
      {hosts.length === 0 && <div className="gnote">{t("ap_no_computers")}</div>}
    </>
  );
}

function About({ version }: { version?: string }) {
  return (
    <>
      <div className="grp">
        <Row ic={<Mark h={14} />} tt="ai-remote" ds={t("ab_what")}>
          <span className="mono" style={{ fontSize: 13 }}>
            {version ?? "—"}
          </span>
        </Row>
        <Row ic={<Icon name="info" />} tt={t("ab_license")} ds={t("ab_license_d")} />
        <Row ic={<Icon name="link" />} tt={t("ab_source")} ds="github.com/JIA-ss/ai-remote" />
      </div>
      <div className="grp-h" style={{ marginTop: 28 }}>
        {t("set_privacy")}
      </div>
      <div className="grp">
        <Row ic={<Icon name="lock" />} tt={t("pv_account")} ds={t("pv_account_d")} />
        <Row ic={<Icon name="eyeOff" />} tt={t("pv_relay")} ds={t("pv_relay_d")} />
        <Row ic={<Icon name="history" />} tt={t("pv_log")} ds={t("pv_log_d")} />
      </div>
    </>
  );
}
