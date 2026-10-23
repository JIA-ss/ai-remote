// Settings, a page of the shell (the sections are a segmented control in its toolbar): General,
// Agent access, This device, Network, Approving and About. Only what works today is shown; nothing
// here is a switch that does nothing.

import { useEffect, useState } from "react";
import { api } from "../lib/api";
import { followSystemLang, langChoice, setLang, t, type Key } from "../lib/i18n";
import { en } from "../lib/strings/en";
import { detectOs, reducedTransparency, setReducedTransparency, setThemeChoice, themeChoice, type ThemeChoice } from "../lib/platform";
import { MCP_CLIENTS, mcpInstall, type McpClient } from "../lib/mcp-install";
import type { Section } from "../lib/nav";
import { useStore } from "../lib/store";
import type {
  AgentStatus,
  AuditLine,
  DoctorCheck,
  Grade,
  Host,
  InstallInfo,
  InstallPlan,
  LocalController,
  LocalHost,
  NetworkSettings,
  TrayIntent,
} from "../lib/types";
import type { NetworkChange } from "../lib/api";
import { shareableAddresses, validAddressInput } from "../lib/network";
import { CmdLine } from "./CmdLine";
import { Confirm } from "./ComputerTabs";
import { Glyph, Mark, Pointer } from "./glyphs";
import { Icon, type IconName } from "./Icon";
import { PairCodeDialog } from "./PairCode";
import { Modal } from "./Modal";
import { clock, cx } from "./util";


const SECTIONS: Array<[Section, IconName | "approval", Key]> = [
  ["general", "settings", "set_general"],
  ["agents", "terminal", "set_agents"],
  ["device", "laptop", "set_device"],
  ["network", "link", "set_network"],
  ["approvals", "approval", "set_approvals"],
  ["about", "info", "set_about"],
];

const mac = detectOs() === "mac";

/** Tray intents are one-shot: each part of one is acted on once, not again when the page remounts. */
const handled = new Set<string>();
function once(part: string, intent: TrayIntent): boolean {
  const key = `${part}/${intent.n}`;
  if (handled.has(key)) return false;
  handled.add(key);
  return true;
}

/** An error for the page: a request the running host does not know means it is older than this app. */
function errorText(e: unknown): string {
  const text = String(e);
  return text.includes("host-outdated") ? t("host_outdated") : text;
}

/** "Oct 22, 14:30": a 24-hour ticket's end, with the day. */

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

/** The settings section's name for the toolbar's screen-reader heading ("Settings: Network"). */
export function settingsTitle(sec: Section): string {
  const label = SECTIONS.find(([s]) => s === sec)?.[2] ?? "set_general";
  return t("settings_h1", { sec: t(label) });
}

/**
 * The sections as one segmented control in the toolbar (the old second sidebar): arrow keys move
 * between them, Home / End jump to the ends. Keeps the `snav` class the screenshots look for.
 */
export function SettingsTabs({ sec, onSec }: { sec: Section; onSec: (sec: Section) => void }) {
  const move = (e: React.KeyboardEvent, i: number) => {
    const to =
      e.key === "ArrowRight" ? (i + 1) % SECTIONS.length
      : e.key === "ArrowLeft" ? (i - 1 + SECTIONS.length) % SECTIONS.length
      : e.key === "Home" ? 0
      : e.key === "End" ? SECTIONS.length - 1
      : -1;
    if (to < 0) return;
    e.preventDefault();
    onSec(SECTIONS[to][0]);
    e.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role=tab]")[to]?.focus();
  };
  return (
    <div className="gl-cap gl-glass thin gl-segcap">
      <nav className="snav gl-seg" role="tablist" aria-label={t("settings")}>
        {SECTIONS.map(([s, ic, label], i) => (
          <button
            key={s}
            type="button"
            role="tab"
            className={cx("gl-segbtn", s === sec && "sel")}
            aria-selected={s === sec}
            tabIndex={s === sec ? 0 : -1}
            title={t(label)}
            onClick={() => onSec(s)}
            onKeyDown={(e) => move(e, i)}
          >
            {ic === "approval" ? <Glyph name="approval" size={14} /> : <Icon name={ic} size={14} />}
            <span className="lbl">{t(label)}</span>
          </button>
        ))}
      </nav>
    </div>
  );
}

/** Settings inside the shell: one section at a time; the section is part of the route. */
export function SettingsPage({
  sec,
  status,
  onStatus,
  intent = null,
}: {
  sec: Section;
  status: AgentStatus | null;
  /** Fetch the agent's status again (after a change here). */
  onStatus: () => void;
  /** A tray intent: its action, anchor or error goes to This device. */
  intent?: TrayIntent | null;
}) {
  const name = status?.name ?? "";
  const heads: Record<Section, [Key, Key]> = {
    general: ["set_general", mac ? "gn_lede_mac" : "gn_lede"],
    agents: ["set_agents", "ag_lede"],
    device: ["set_device", "dv_lede"],
    network: ["set_network", "nw_lede"],
    approvals: ["set_approvals", "ap_lede"],
    about: ["set_about", "ab_lede"],
  };
  const [hd, lede] = heads[sec];
  return (
    <div className="spage" style={{ overflowY: "auto" }}>
      <div className="hd">{t(hd)}</div>
      <div className="body c2" style={{ marginTop: 6, maxWidth: 640 }}>
        {t(lede, { name })}
      </div>
      <div style={{ marginTop: 28 }}>
        {sec === "general" && <General version={status?.version} />}
        {sec === "agents" && <Agents status={status} name={name} onChange={onStatus} />}
        {sec === "device" && <Device status={status} intent={intent} />}
        {sec === "network" && <Network />}
        {sec === "approvals" && <Approvals />}
        {sec === "about" && <About version={status?.version} />}
      </div>
    </div>
  );
}

function General({ version }: { version?: string }) {
  const [choice, setChoice] = useState(langChoice());
  const [appearance, setAppearance] = useState(themeChoice());
  const [opaque, setOpaque] = useState(reducedTransparency());
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
      <Row ic={<Icon name="monitor" />} tt={t("gn_theme_choice")}>
        <span className="dd-wrap">
          <select className="dd" aria-label={t("gn_theme_choice")} value={appearance} onChange={(e) => {
            const next = e.target.value as ThemeChoice;
            setAppearance(next);
            setThemeChoice(next);
          }}>
            <option value="system">{t("gn_theme_system")}</option>
            <option value="light">{t("gn_theme_light")}</option>
            <option value="dark">{t("gn_theme_dark")}</option>
          </select>
          <Icon name="chevD" size={12} />
        </span>
      </Row>
      <Row ic={<Icon name="eye" />} tt={t("gn_transparency")} ds={t("gn_transparency_d")}>
        <span className="dd-wrap">
          <select className="dd" aria-label={t("gn_transparency")} value={opaque ? "reduced" : "full"} onChange={(e) => {
            const next = e.target.value === "reduced";
            setOpaque(next);
            setReducedTransparency(next);
          }}>
            <option value="full">{t("gn_glass_on")}</option>
            <option value="reduced">{t("gn_glass_off")}</option>
          </select>
          <Icon name="chevD" size={12} />
        </span>
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

function Device({ status, intent }: { status: AgentStatus | null; intent: TrayIntent | null }) {
  const [copied, setCopied] = useState(false);
  const id = status?.device ?? "";
  // This computer as a host: its paired controllers when ai-remote host runs here, `null` when not.
  const [controllers, setControllers] = useState<LocalController[] | null | undefined>(undefined);
  const [hostError, setHostError] = useState<string | null>(null);
  const reload = () =>
    api.localControllers().then(setControllers, (e) => {
      setControllers(null);
      setHostError(String(e));
    });
  useEffect(() => {
    void reload();
  }, []);
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
      <HostPanel name={status?.name ?? ""} intent={intent} onChanged={() => void reload()}>
        {controllers !== undefined && <TrustedControllers list={controllers} hostError={hostError} reload={reload} />}
        {controllers && <LocalAgents />}
      </HostPanel>
    </>
  );
}

/**
 * Whether paired devices may start coding agents on this computer (see the chat tab beside a live
 * view). Set over the host's local channel only; switching off ends the running sessions.
 */
function LocalAgents() {
  const [state, setState] = useState<{ enabled: boolean; live: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = () => api.localChat().then((v) => setState(v ? { enabled: v.enabled, live: v.live } : null), (e) => setError(String(e)));
  useEffect(() => {
    void load();
  }, []);
  if (!state) return error ? <div className="cap c-danger" style={{ marginTop: 10 }}>{error}</div> : null;
  return (
    <>
      <div className="grp-h" style={{ marginTop: 28 }}>
        {t("hc_set_h")}
      </div>
      <div className="grp">
        <Row
          ic={<Icon name="terminal" />}
          tt={t("hc_set_t")}
          ds={state.enabled ? (state.live ? t("hc_set_d_live", { n: state.live }) : t("hc_set_d_on")) : t("hc_set_d_off")}
        >
          <span className="dd-wrap">
            <select
              className="dd"
              aria-label={t("hc_set_t")}
              value={state.enabled ? "on" : "off"}
              onChange={(e) => {
                setError(null);
                api.setLocalChat(e.target.value === "on").then(load, (err) => setError(String(err)));
              }}
            >
              <option value="on">{t("hc_set_on")}</option>
              <option value="off">{t("hc_set_off")}</option>
            </select>
            <Icon name="chevD" size={12} />
          </span>
        </Row>
      </div>
      {error && <div className="cap c-danger" style={{ marginTop: 10 }}>{error}</div>}
    </>
  );
}

const GRADE_COLOR: Record<Grade, string> = { ok: "var(--success)", warn: "var(--relay)", fail: "var(--danger)", skip: "var(--idle)" };

/** A small round grade mark, where a row's icon goes. */
function GradeDot({ grade }: { grade: Grade }) {
  return <i role="img" aria-label={t(`grade_${grade}` as Key)} style={{ display: "inline-block", width: 10, height: 10, borderRadius: 5, background: GRADE_COLOR[grade] }} />;
}

/** Checks shown as permissions and session facts; the rest (network, disk, bypass) has its own places. */
const PERMISSION_CHECKS = ["screen_recording", "accessibility", "session", "console", "desktop", "elevation", "approval", "firewall"];
/** Checks with a System Settings pane to open (macOS privacy panes). */
const SETTINGS_PANE: Record<string, string> = { screen_recording: "screen_recording", accessibility: "accessibility" };

/** A check's finding in the page's language where the catalog has it, else the host's own words. */
const checkSummary = (c: DoctorCheck) => {
  const key = `ckd_${c.id}_${c.grade === "ok" ? "ok" : "bad"}` as Key;
  return key in en ? t(key) : c.summary;
};

const checkName = (id: string) => {
  const key = `ck_${id}` as Key;
  return key in en ? t(key) : id;
};

/**
 * This computer as a host: whether it runs, who is connected and who drives, with the owner's
 * controls (pause, stop / start, a pairing code); its permissions as the running host sees them;
 * the autostart entry; where the approval rules live; and the last audit lines. The same facts
 * the tray icon and menu show, in full.
 */
function HostPanel({
  name,
  intent,
  onChanged,
  children,
}: {
  name: string;
  intent: TrayIntent | null;
  onChanged: () => void;
  /** Shown right after the host's state: the paired controllers. */
  children?: React.ReactNode;
}) {
  const [lh, setLh] = useState<LocalHost | null | undefined>(undefined);
  // The checks are slow to gather (autostart, firewall): asked for on opening and after a change,
  // while the status alone is polled.
  const [doctor, setDoctor] = useState<LocalHost["doctor"]>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pairOpen, setPairOpen] = useState(false);
  const [recent, setRecent] = useState<AuditLine[] | null>(null);
  const [round, setRound] = useState(0);
  const [loginItem, setLoginItem] = useState<boolean | null>(null);
  const [askStop, setAskStop] = useState(false);
  const [install, setInstall] = useState<InstallInfo | null | undefined>(undefined);
  /** The enable dialog: with the login window / lock screen (`true`) or after login only. */
  // `?shotdialog=enable`: opens the enable dialog (screenshots of the page outside the app).
  const [enable, setEnable] = useState<boolean | null>(() => (new URLSearchParams(location.search).get("shotdialog") === "enable" ? true : null));
  const [askDisable, setAskDisable] = useState(false);
  /** The permission being granted, and what the page says meanwhile. */
  const [granting, setGranting] = useState<string | null>(null);
  const [grantNote, setGrantNote] = useState<Key | null>(null);
  /** What a fresh copy of the host is granted (macOS): a grant the running host still holds may be gone. */
  const [freshGrants, setFreshGrants] = useState<Record<string, boolean> | null>(null);
  useEffect(() => {
    api.appLoginItem().then(setLoginItem, () => setLoginItem(null));
  }, []);
  const running = !!lh?.host;
  useEffect(() => {
    let live = true;
    api.localHost(true).then(
      (v) => {
        if (!live) return;
        setLh(v);
        setDoctor(v.doctor);
        setInstall(v.install ?? null);
      },
      () => live && setLh(null),
    );
    let wasRunning: boolean | null = null;
    const timer = setInterval(() => {
      api.localHost(false).then((v) => {
        if (!live) return;
        setLh(v);
        // Started or stopped from elsewhere (the tray, a terminal): the checks change with it.
        const running = !!v.host;
        if (wasRunning !== null && running !== wasRunning) api.localHost(true).then((w) => {
          if (!live) return;
          setDoctor(w.doctor);
          setInstall(w.install ?? null);
        }, () => {});
        wasRunning = running;
      }, () => {});
    }, 5000);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [round]);
  useEffect(() => {
    if (!running) {
      setRecent(null);
      return;
    }
    let live = true;
    if (!lh?.host?.ops?.includes("recent")) {
      setRecent([]);
      return;
    }
    api.localRecent(20).then((v) => live && setRecent(v), () => live && setRecent([]));
    return () => {
      live = false;
    };
  }, [running, round]);
  // What the tray menu asked for.
  const [anchor, setAnchor] = useState<string | null>(null);
  useEffect(() => {
    if (!intent || !once("host", intent)) return;
    if (intent.action === "pair_code") setPairOpen(true);
    if (intent.action === "confirm_stop") setAskStop(true);
    if (intent.action === "enable") setEnable(true);
    if (intent.error) setError(intent.error);
    if (intent.anchor) setAnchor(intent.anchor);
  }, [intent]);
  /** Runs an action, then keeps the buttons busy until the page shows its result. */
  const refresh = async () => {
    // The host needs a moment to change (a stop takes a second or two).
    await new Promise((r) => setTimeout(r, 700));
    const v = await api.localHost(true).catch(() => null);
    if (v) {
      setLh(v);
      setDoctor(v.doctor);
      setInstall(v.install ?? null);
    }
    setRound((r) => r + 1);
    onChanged();
  };
  const act = async (f: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
      await refresh();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  /**
   * One click per permission: the host asks macOS itself (its own grant, inside this app), the
   * person allows it in the system's dialog or pane, the page notices from a fresh copy of the host
   * and restarts the host when it needs that (Screen Recording), then shows the checks again.
   */
  const grant = async (permission: string) => {
    setGranting(permission);
    setGrantNote("grant_waiting");
    setError(null);
    try {
      await api.requestPermission(permission);
      for (let i = 0; i < 90; i++) {
        await new Promise((r) => setTimeout(r, 2000));
        const p = await api.hostPermissions().catch(() => null);
        if (!p?.fresh?.[permission]) continue;
        if (!p.running?.[permission]) {
          setGrantNote("grant_restarting");
          await api.localRestart();
          for (let j = 0; j < 30; j++) {
            await new Promise((r) => setTimeout(r, 1000));
            const v = await api.localHost(false).catch(() => null);
            if (v?.host) break;
          }
        }
        await refresh();
        setGrantNote(null);
        setGranting(null);
        return;
      }
      setGrantNote("grant_timeout");
    } catch (e) {
      setError(errorText(e));
      setGrantNote(null);
    }
    setGranting(null);
  };
  // Scroll to what the tray asked for once the sections above it have their content.
  useEffect(() => {
    if (!anchor || lh === undefined || (lh?.host && recent === null)) return;
    const timer = setTimeout(() => {
      document.getElementById(anchor)?.scrollIntoView({ behavior: "smooth", block: "start" });
      setAnchor(null);
    }, doctor ? 50 : 1500);
    return () => clearTimeout(timer);
  }, [anchor, lh, doctor, recent]);
  const host = lh?.host ?? null;
  /** Whether the running host understands `op` (hosts older than the `ops` list understand none of the new ones). */
  const can = (op: string) => !!host?.ops?.includes(op);
  const outdated = !!host && !host.ops;
  const paused = host?.driver?.state === "paused";
  const connected = host?.connected ?? [];
  const facts: string[] = [];
  if (host) {
    facts.push(t("lh_running_d", { pid: host.pid, version: host.version, where: t(host.background ? "lh_where_bg" : "lh_where_term") }));
    const who = host.controllers?.length
      ? host.controllers
          .map((c) => t("lh_conn_item", { name: c.name, channels: c.channels.map((ch) => t(`ch_${ch}` as Key)).join(t("list_sep")) }))
          .join(t("item_sep"))
      : connected.join(t("list_sep"));
    facts.push(connected.length ? t("lh_connected", { n: connected.length, names: who }) : t("lh_connected_none"));
    const d = host.driver ?? { state: "agent" as const };
    facts.push(
      d.state === "person"
        ? t("lh_driver_person", { device: d.device })
        : d.state === "paused"
          ? d.owner
            ? t("lh_driver_paused")
            : t("lh_driver_paused_by", { by: d.by })
          : t("lh_driver_agent"),
    );
    const busy = host.calls?.at(-1);
    if (busy && d.state !== "paused") facts.push(t(busy.person ? "lh_busy_person" : "lh_busy_agent", { peer: busy.peer, tool: busy.tool }));
    if (host.prompt_blocker) facts.push(t("lh_prompt_blocked", { why: host.prompt_blocker }));
  }
  useEffect(() => {
    if (!mac || !lh?.host?.ops?.includes("permissions") || !doctor) return;
    let live = true;
    api.hostPermissions().then((p) => live && setFreshGrants(p.fresh), () => {});
    return () => {
      live = false;
    };
  }, [doctor, lh?.host?.pid]);
  const checks = (doctor?.checks ?? [])
    .filter((c) => c.grade !== "skip")
    // The running host still holds a grant that a fresh copy no longer gets (the app was rebuilt or
    // the entry changed): it is lost at the next restart.
    .map((c) =>
      c.grade === "ok" && (c.id === "screen_recording" || c.id === "accessibility") && freshGrants?.[c.id] === false
        ? { ...c, grade: "warn" as Grade, summary: t("grant_stale") }
        : c,
    );
  const permissions = checks.filter((c) => PERMISSION_CHECKS.includes(c.id));
  const autostart = checks.find((c) => c.id === "autostart");
  return (
    <>
      <div className="grp-h" style={{ marginTop: 28 }}>
        {t("lh_h")}
      </div>
      <div className="grp">
        {lh === undefined ? (
          <Row ic={<Icon name="laptop" />} tt="…" />
        ) : !host && lh?.unknown ? (
          <Row ic={<GradeDot grade="warn" />} tt={t("lh_unknown")} ds={t("lh_unknown_d", { why: lh.unknown })} />
        ) : (
          <Row
            ic={<GradeDot grade={host ? (paused ? "warn" : "ok") : "skip"} />}
            tt={host ? t("lh_running") : t("lh_not_running")}
            ds={host ? facts.map((f, i) => <div key={i}>{f}</div>) : t("lh_not_running_d")}
          >
            {host ? (
              <>
                <button className="btn" disabled={busy || !can("pause")} onClick={() => void act(() => api.localPause(!paused))}>
                  <Icon name={paused ? "play" : "pause"} size={14} />
                  {t(paused ? "lh_resume" : "lh_pause")}
                </button>
                <button className="btn" disabled={busy} onClick={() => (host.system ? setAskStop(true) : void act(() => api.localStop()))}>
                  <Icon name="stop" size={14} />
                  {t("lh_stop")}
                </button>
              </>
            ) : (
              install && !install.installed ? (
                <button className="btn primary" onClick={() => setEnable(true)}>
                  {t("en_btn")}
                </button>
              ) : (
                <button className="btn primary" disabled={busy} onClick={() => void act(() => api.localStart())}>
                  <Icon name="play" size={14} />
                  {t("lh_start")}
                </button>
              )
            )}
          </Row>
        )}
        {outdated && (
          <Row ic={<GradeDot grade="warn" />} tt={t("host_old_h")} ds={t("host_old_d")}>
            <button className="btn primary" onClick={() => setEnable(install?.service ?? true)}>
              {t("host_old_btn")}
            </button>
          </Row>
        )}
        {host && (
          <Row ic={<Icon name="qr" />} tt={t("lh_pair")} ds={t("lh_pair_d")}>
            <button className="btn" onClick={() => setPairOpen(true)}>
              {t("lh_pair_btn")}
            </button>
          </Row>
        )}
      </div>
      {error && <div className="cap c-danger" style={{ marginTop: 10 }}>{t("lh_error", { error })}</div>}
      {children}

      {doctor && permissions.length > 0 && (
        <>
          <div className="grp-h" style={{ marginTop: 28 }}>
            {t("perm_h")}
          </div>
          <div className="grp-sub">{t(doctor.checked_in === "host" ? "perm_h_host" : "perm_h_cli")}</div>
          <div className="grp">
            {permissions.map((c) => (
              <CheckRow
                key={c.id}
                check={c}
                onGrant={host && mac && can("request_permission") && (c.id === "screen_recording" || c.id === "accessibility") ? () => void grant(c.id) : undefined}
                granting={granting === c.id ? grantNote : null}
                busy={granting !== null}
              />
            ))}
          </div>
        </>
      )}

      <div className="grp-h" style={{ marginTop: 28 }}>
        {t("as_h")}
      </div>
      <div className="grp">
        {install && !install.installed ? null : autostart ? (
          <CheckRow check={autostart} />
        ) : (
          <Row ic={<GradeDot grade="skip" />} tt={t("as_h")} ds="—" />
        )}
        {install && !install.installed && (
          <Row ic={<Icon name="monitor" />} tt={t("en_h")} ds={t(mac ? "en_d_mac" : "en_d_win")}>
            <button className="btn" onClick={() => setEnable(false)}>
              {t("en_user_btn")}
            </button>
            <button className="btn primary" onClick={() => setEnable(true)}>
              {t("en_btn")}
            </button>
          </Row>
        )}
        {install?.installed && install.disabled && (
          <Row ic={<GradeDot grade="warn" />} tt={t("as_off_h")} ds={t("as_off_d")}>
            <button className="btn primary" disabled={busy} onClick={() => void act(() => api.localStart())}>
              {t("as_off_btn")}
            </button>
          </Row>
        )}
        {install?.installed && install.current === false && (
          <Row ic={<GradeDot grade="warn" />} tt={t("en_other_h")} ds={t("en_other_d", { what: install.launches ?? install.name })}>
            <button className="btn primary" onClick={() => setEnable(!!install.service)}>
              {t("en_other_btn")}
            </button>
          </Row>
        )}
        {install?.installed && mac && !install.service && (
          <Row ic={<Icon name="lock" />} tt={t("en_lw_h")} ds={t("en_lw_d")}>
            <button className="btn" onClick={() => setEnable(true)}>
              {t("en_lw_btn")}
            </button>
          </Row>
        )}
        {install?.installed && (
          <Row ic={<Icon name="x" />} tt={t("dis_h")} ds={t(install.service ? "dis_d_service" : "dis_d")}>
            <button className="btn" onClick={() => setAskDisable(true)}>
              {t("dis_btn")}
            </button>
          </Row>
        )}
        <Row ic={<Mark h={12} />} tt={t(mac ? "li_mac" : "li_win")} ds={t("li_d")}>
          <span className="dd-wrap">
            <select
              className="dd"
              aria-label={t(mac ? "li_mac" : "li_win")}
              value={loginItem ? "on" : "off"}
              disabled={loginItem === null}
              onChange={(e) => {
                const on = e.target.value === "on";
                setError(null);
                api.setAppLoginItem(on).then(() => setLoginItem(on), (err) => setError(errorText(err)));
              }}
            >
              <option value="off">{t("li_off")}</option>
              <option value="on">{t("li_on")}</option>
            </select>
            <Icon name="chevD" size={12} />
          </span>
        </Row>
      </div>

      <div className="grp-h" style={{ marginTop: 28 }}>
        {t("rules_h")}
      </div>
      <div className="grp">
        <Row ic={<Icon name="shield" />} tt={t("rules_h")} ds={t("rules_d")}>
          <button className="btn" onClick={() => void api.revealProfile().catch((e) => setError(errorText(e)))}>
            <Icon name="open" size={14} />
            {t(mac ? "rules_open_mac" : "rules_open_win")}
          </button>
        </Row>
      </div>

      <div className="grp-h" id="recent" style={{ marginTop: 28 }}>
        {t("recent_h")}
        {host && (
          <button className="btn icon end" aria-label={t("recent_refresh")} onClick={() => setRound((r) => r + 1)}>
            <Icon name="refresh" size={14} />
          </button>
        )}
      </div>
      <div className="grp-sub">{t("recent_d")}</div>
      {!host ? (
        <div className="gnote" style={{ marginTop: 0 }}>{t("recent_no_host")}</div>
      ) : !recent || recent.length === 0 ? (
        <div className="gnote" style={{ marginTop: 0 }}>{recent ? t("recent_none") : "…"}</div>
      ) : (
        <div className="grp">
          {[...recent].reverse().map((l, i) => (
            <div key={`${l.ts}-${i}`} className="gr noic" style={{ minHeight: 36, padding: "8px 16px" }}>
              <div className="ds" style={{ display: "flex", gap: 12, alignItems: "baseline", minWidth: 0 }}>
                <span className="mono" style={{ fontSize: 12, color: "var(--text-3)", flex: "none" }}>
                  {clock(l.ts)}
                </span>
                <span className="ell" style={{ color: "var(--text-1)" }}>{auditText(l)}</span>
              </div>
            </div>
          ))}
        </div>
      )}
      {enable !== null && (
        <EnableDialog
          service={enable}
          onClose={() => setEnable(null)}
          onDone={async () => {
            setEnable(null);
            // launchd / the service starts the host; wait for it to answer, then show everything again.
            for (let i = 0; i < 30; i++) {
              const v = await api.localHost(false).catch(() => null);
              if (v?.host) break;
              await new Promise((r) => setTimeout(r, 1000));
            }
            await refresh();
          }}
        />
      )}
      {askDisable && (
        <Confirm
          title={t("dis_q")}
          body={t(install?.service ? "dis_body_service" : "dis_body")}
          verb={t("dis_btn")}
          danger
          onNo={() => setAskDisable(false)}
          onYes={async () => {
            try {
              await api.hostSetup(false, false, t("dis_prompt"));
            } catch (e) {
              if (String(e).includes("cancelled")) {
                setAskDisable(false);
                return;
              }
              throw e;
            }
            setAskDisable(false);
            await refresh();
          }}
        />
      )}
      {pairOpen && <PairCodeDialog name={name} connected={connected} onClose={() => setPairOpen(false)} />}
      {askStop && (
        <Confirm
          title={t("lh_stop_q")}
          body={t(host?.system ? "lh_stop_body_service" : "lh_stop_body")}
          verb={t("lh_stop")}
          danger
          onNo={() => setAskStop(false)}
          onYes={async () => {
            await api.localStop();
            setAskStop(false);
            await refresh();
          }}
        />
      )}
    </>
  );
}

/**
 * Settings > Network, for this computer as a host and as a controller alike: how devices connect
 * (automatic, or the local network only), and two opt-ins that put traffic on the network beyond the
 * connections (local-network discovery, router port mapping), which `quiet = true` in ai-remote.toml
 * keeps off. The controller applies a change at once, a host running here when it next starts. With
 * the local network only, this computer's address and each paired computer's are shown, the latter
 * editable for a computer whose IP changed.
 */
function Network() {
  const [net, setNet] = useState<NetworkSettings | null>(null);
  const [hosts, setHosts] = useState<Host[]>([]);
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState<Key | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.networkSettings().then(setNet, (e) => setError(String(e)));
    api.hosts().then(setHosts, () => setHosts([]));
  }, []);
  const change = async (c: NetworkChange) => {
    if (busy || !net) return;
    setBusy(true);
    setError(null);
    try {
      const next = await api.updateNetwork(c);
      setNet(next);
      setSaved(next.host_running ? "nw_saved_host" : "nw_saved");
    } catch (e) {
      setError(String(e));
      setSaved(null);
    } finally {
      setBusy(false);
    }
  };
  const lan = net?.mode === "lan";
  const port = net?.lan_port ?? 47800;
  const here = shareableAddresses(net?.host_addresses ?? []);
  const modes: Array<[NetworkSettings["mode"], Key, Key]> = [
    ["auto", "nw_auto", "nw_auto_d"],
    ["lan", "nw_lan", "nw_lan_d"],
  ];
  return (
    <>
      <div className="grp-h">{t("nw_mode")}</div>
      <div className="grp" role="radiogroup" aria-label={t("nw_mode")}>
        {modes.map(([m, tt, ds]) => {
          const on = net?.mode === m;
          const pick = () => {
            if (!on) void change({ mode: m });
          };
          return (
            <div
              key={m}
              className={cx("gr", !on && "pick")}
              role="radio"
              aria-checked={on}
              tabIndex={0}
              onClick={pick}
              onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && pick()}
            >
              <span className="ic bare">
                <span className={cx("radio", on && "on")} />
              </span>
              <div>
                <div className="tt">
                  {t(tt)}
                  {m === "auto" && <span className="rec">{t("nw_recommended")}</span>}
                </div>
                <div className="ds">{t(ds)}</div>
              </div>
              <span className="end" />
            </div>
          );
        })}
      </div>
      {saved && (
        <div className="gnote" role="status">
          {t(saved)}
        </div>
      )}
      {error && (
        <div className="gnote c-danger" role="alert">
          {error}
        </div>
      )}

      <div className="grp-h" style={{ marginTop: 28 }}>
        {t("nw_extras")}
      </div>
      <div className="grp-sub">{t(net?.locked ? "nw_locked" : "nw_extras_sub")}</div>
      <div className="grp">
        <Toggle
          icon="search"
          title={t("nw_find")}
          desc={t("nw_find_d")}
          on={net?.lan_discovery ?? false}
          disabled={!net || net.locked || busy}
          onChange={(on) => void change({ lanDiscovery: on })}
        />
        {!lan && (
          <Toggle
            icon="open"
            title={t("nw_upnp")}
            desc={t("nw_upnp_d")}
            on={net?.port_mapping ?? false}
            disabled={!net || net.locked || busy}
            onChange={(on) => void change({ portMapping: on })}
          />
        )}
      </div>

      {lan && net?.host_running && (
        <>
          <div className="grp-h" style={{ marginTop: 28 }}>
            {t("nw_here")}
          </div>
          <div className="grp">
            <Row
              ic={<Icon name="laptop" />}
              tt={net.host_lan && here.length > 0 ? <span className="mono">{here[0]}</span> : t("nw_here_next", { port })}
              ds={
                net.host_lan && here.length > 1 ? (
                  <>
                    {t("nw_here_d")} <span className="mono">{here.slice(1).join(", ")}</span>
                  </>
                ) : (
                  t("nw_here_d")
                )
              }
            >
              {net.host_lan && here.length > 0 && <CopyText text={here[0]} />}
            </Row>
          </div>
        </>
      )}

      {lan && hosts.length > 0 && (
        <>
          <div className="grp-h" style={{ marginTop: 28 }}>
            {t("nw_peers")}
          </div>
          <div className="grp-sub">{t(net?.lan_discovery ? "nw_peers_found" : "nw_peers_d", { port })}</div>
          <div className="grp">
            {hosts.map((h) => (
              <HostAddress key={h.name} host={h} />
            ))}
          </div>
        </>
      )}
    </>
  );
}

/** A settings row with an on/off switch at its end. */
function Toggle({
  icon,
  title,
  desc,
  on,
  disabled,
  onChange,
}: {
  icon: IconName;
  title: string;
  desc: string;
  on: boolean;
  disabled: boolean;
  onChange: (on: boolean) => void;
}) {
  return (
    <Row ic={<Icon name={icon} />} tt={title} ds={desc}>
      <button
        className={cx("switch", on && "on")}
        role="switch"
        aria-checked={on}
        aria-label={title}
        disabled={disabled}
        onClick={() => onChange(!on)}
      />
    </Row>
  );
}

/** A copy button that says so for a moment after copying. */
function CopyText({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      className="btn"
      onClick={() => {
        void navigator.clipboard?.writeText(text);
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      }}
    >
      {t(copied ? "nw_copied" : "nw_copy")}
    </button>
  );
}

/** Where this device reaches one paired computer, and a way to change it. */
function HostAddress({ host }: { host: Host }) {
  const [addrs, setAddrs] = useState(host.addresses ?? []);
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    if (!validAddressInput(value)) {
      setError(t("nw_bad_addr"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      setAddrs(await api.setHostAddress(host.name, value));
      setEditing(false);
      setValue("");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const shown = addrs.length > 0 ? <span className="mono">{shareableAddresses(addrs).join(", ") || addrs.join(", ")}</span> : t("nw_none");
  return (
    <Row ic={<Icon name="monitor" />} tt={host.name} ds={error ? <span className="c-danger">{error}</span> : shown}>
      {editing ? (
        <>
          <input
            className="field"
            style={{ width: 190 }}
            value={value}
            autoFocus
            placeholder={t("nw_addr_ph")}
            aria-label={t("nw_addr_for", { host: host.name })}
            onChange={(e) => {
              setValue(e.target.value);
              setError(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") void save();
              if (e.key === "Escape") setEditing(false);
            }}
          />
          <button className="btn" disabled={busy} onClick={() => void save()}>
            {t("nw_save")}
          </button>
          <button
            className="btn"
            onClick={() => {
              setEditing(false);
              setError(null);
            }}
          >
            {t("nw_cancel")}
          </button>
        </>
      ) : (
        <button className="btn" onClick={() => setEditing(true)}>
          {t("nw_edit")}
        </button>
      )}
    </Row>
  );
}

/** A value from the audit log in the page's language where the catalog has it (`dec_approved`, ...). */
function word(prefix: string, v: string | undefined): string {
  if (!v) return "";
  const key = `${prefix}_${v}` as Key;
  return key in en ? t(key) : v;
}

/** One audit line as a sentence; names from the host are quoted as they are. */
function auditText(l: AuditLine): string {
  const peer = l.peer ?? "?";
  switch (l.kind) {
    case "session_start":
      return t("a_session_start", { peer, client: word("cl", typeof l.client === "string" ? l.client : undefined) });
    case "session_end":
      return t("a_session_end", { peer });
    case "decision":
      return (l.intent ? `${l.intent} — ` : "") + t("a_decision", { peer, tool: l.tool ?? "", decision: word("dec", l.decision) });
    case "outcome":
      return t("a_outcome", { peer, tool: l.tool ?? "", outcome: word("res", l.outcome) }) + (l.error ? `: ${l.error}` : "");
    case "view_start":
      return t("a_view_start", { peer });
    case "view_end":
      return t("a_view_end", { peer });
    case "paired":
      return t("a_paired", { peer, level: String(l.level ?? ""), sas: l.sas ?? "" });
    case "driver":
      if (l.origin === "owner") return t(l.state === "paused" ? "a_owner_paused" : "a_owner_resumed");
      return l.state === "person" ? t("a_driver_person", { device: l.device ?? l.by ?? "" }) : l.state === "paused" ? t("a_driver_paused", { by: l.by ?? "" }) : t("a_driver_agent", { by: l.by ?? "" });
    case "bypass":
      if (l.default !== undefined && !l.peer) return t(l.on ? "a_bypass_default_on" : "a_bypass_default_off");
      return t(l.on ? "a_bypass_on" : "a_bypass_off", { peer });
    default:
      return `${l.kind}${l.peer ? ` · ${l.peer}` : ""}`;
  }
}

function CheckRow({
  check,
  onGrant,
  granting = null,
  busy = false,
}: {
  check: DoctorCheck;
  /** One click: the host asks macOS for this permission (macOS, host running). */
  onGrant?: () => void;
  /** What the page says while this permission is being granted. */
  granting?: Key | null;
  busy?: boolean;
}) {
  // macOS lists LaunchAgents under Login Items; Windows' autostart is a task or a service, which no
  // Settings page shows, so there is no button for it there.
  const pane = mac ? (check.id === "autostart" ? "login_items" : SETTINGS_PANE[check.id]) : undefined;
  const [error, setError] = useState<string | null>(null);
  return (
    <Row
      ic={<GradeDot grade={check.grade} />}
      tt={checkName(check.id)}
      ds={
        <>
          <div>{checkSummary(check)}</div>
          {check.grade !== "ok" && checkSummary(check) !== check.summary && <div style={{ marginTop: 2 }}>{check.summary}</div>}
          {/* The page has buttons for what the host's fix would have you type. */}
          {check.grade !== "ok" && check.fix && check.id !== "autostart" && !onGrant && <div style={{ marginTop: 2 }}>{t("perm_fix", { fix: check.fix })}</div>}
          {granting && <div style={{ marginTop: 4, color: "var(--agent)" }}>{t(granting)}</div>}
          {error && <div className="c-danger">{error}</div>}
        </>
      }
    >
      {onGrant && check.grade !== "ok" && (
        <button className="btn primary" disabled={busy} onClick={onGrant}>
          {t("grant_btn")}
        </button>
      )}
      {pane && (check.grade !== "ok" || check.id === "autostart") && (
        <button className="btn" onClick={() => void api.openSystemSettings(pane).catch((e) => setError(errorText(e)))}>
          <Icon name="open" size={14} />
          {t(check.id === "autostart" ? "as_open" : "perm_open")}
        </button>
      )}
    </Row>
  );
}

/**
 * Enabling remote control here from the app: what will change (the dry-run plan), then the system's
 * own administrator prompt (macOS password, Windows UAC) runs the bundled `ai-remote host install`,
 * so the autostart entry runs the host inside this app. No terminal.
 */
function EnableDialog({ service, onClose, onDone }: { service: boolean; onClose: () => void; onDone: () => Promise<void> }) {
  const [plan, setPlan] = useState<InstallPlan | null>(null);
  const [state, setState] = useState<"plan" | "running" | "error">("plan");
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.hostInstallPlan(service).then(setPlan, (e) => {
      setError(errorText(e));
      setState("error");
    });
  }, [service]);
  const admin = service || !mac;
  const go = async () => {
    setState("running");
    setError(null);
    try {
      await api.hostSetup(true, service, t(service ? "en_prompt_lw" : "en_prompt"));
      await onDone();
    } catch (e) {
      const text = String(e);
      setError(text.includes("cancelled") ? t("en_cancelled") : text);
      setState("error");
    }
  };
  // The plan's own blocker ("needs root") is what the administrator prompt answers.
  const steps = plan?.steps ?? [];
  return (
    <Modal label={t(service ? "en_title_lw" : "en_title")} onClose={onClose} dismissible={state !== "running"} size="wide">
        <div className="b">
          <div className="ttl">{t(service ? "en_title_lw" : "en_title")}</div>
          <div className="bd">
            <div>{t(service ? (mac ? "en_what_lw_mac" : "en_what_lw_win") : mac ? "en_what_mac" : "en_what_win")}</div>
            {plan ? (
              <div style={{ marginTop: 14, maxHeight: 240, overflowY: "auto", padding: "10px 12px", borderRadius: "var(--r-control)", background: "var(--well)" }}>
                <div className="cap" style={{ color: "var(--text-1)", marginBottom: 6 }}>{t("en_changes")}</div>
                {steps.map((s, i) => (
                  <div key={i} className="cap mono" style={{ fontSize: 11, color: "var(--text-2)", marginBottom: 4, wordBreak: "break-all" }}>
                    {i + 1}. {s.text.split("\n")[0]}
                  </div>
                ))}
                {plan.notes.map((n, i) => (
                  <div key={`n${i}`} className="cap" style={{ color: "var(--text-3)", marginTop: 6 }}>
                    · {n}
                  </div>
                ))}
              </div>
            ) : (
              state !== "error" && <div className="cap c3" style={{ marginTop: 14 }}>…</div>
            )}
            {state === "running" && <div className="cap" style={{ marginTop: 12, color: "var(--agent)" }}>{t(admin ? "en_running_admin" : "en_running")}</div>}
            {error && <div className="cap c-danger" style={{ marginTop: 12, whiteSpace: "pre-wrap" }}>{error}</div>}
          </div>
        </div>
        <div className="f">
          <button className="btn primary" disabled={!plan || state === "running"} onClick={() => void go()}>
            {t(admin ? (mac ? "en_go_mac" : "en_go_win") : "en_go")}
          </button>
          <button className="btn" disabled={state === "running"} onClick={onClose}>
            {t("cancel")}
          </button>
        </div>
    </Modal>
  );
}

const levelKey = { observe: "lvl_observe", control: "lvl_control", admin: "lvl_admin" } as const;

/**
 * This computer as a host: the controllers paired with it, each either asking first (the default)
 * or trusted with everything (bypass). Set over the host's local channel only, so no controller or
 * agent can give it to itself; turning it on asks for a confirmation that says what it means.
 */
function TrustedControllers({
  list,
  hostError,
  reload,
}: {
  list: LocalController[] | null;
  hostError: string | null;
  reload: () => Promise<void>;
}) {
  const [ask, setAsk] = useState<LocalController | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = reload;
  const describe = (c: LocalController) => {
    if (c.revoked) return t("bp_d_revoked");
    if (c.level === null) return t("bp_d_nolevel");
    if (c.bypass) return c.until_ms ? t("bp_d_until", { time: clock(c.until_ms, false) }) : t("bp_d_on");
    return t("bp_d_off", { level: t(levelKey[c.level]) });
  };
  return (
    <>
      <div className="grp-h" style={{ marginTop: 28 }}>
        {t("bp_h")}
      </div>
      <div className="grp-sub">{t("bp_sub")}</div>
      {list === null ? (
        <div className="gnote" style={{ marginTop: 0 }}>
          {hostError ?? t("bp_no_host")}
        </div>
      ) : list.length === 0 ? (
        <div className="gnote" style={{ marginTop: 0 }}>
          {t("bp_none")}
        </div>
      ) : (
        <div className="grp">
          {list.map((c) => (
            <Row
              key={c.name}
              ic={<Icon name="laptop" />}
              tt={c.name}
              ds={<span className={cx(c.bypass && "c-danger")}>{describe(c)}</span>}
            >
              <span className="dd-wrap">
                <select
                  className="dd"
                  aria-label={t("bp_h")}
                  value={c.bypass ? "all" : "ask"}
                  disabled={!c.bypass && (c.revoked || c.level === null)}
                  onChange={(e) => {
                    setError(null);
                    if (e.target.value === "all") {
                      setAsk(c);
                    } else {
                      api.setLocalBypass(c.name, false).then(load, (err) => setError(errorText(err)));
                    }
                  }}
                >
                  <option value="ask">{t("bp_v_ask")}</option>
                  <option value="all">{t("bp_v_all")}</option>
                </select>
                <Icon name="chevD" size={12} />
              </span>
            </Row>
          ))}
        </div>
      )}
      {error && list !== null && <div className="cap c-danger" style={{ marginTop: 10 }}>{error}</div>}
      {ask && (
        <Confirm
          title={t("bp_q", { name: ask.name })}
          body={t("bp_body", { name: ask.name, level: ask.level ? t(levelKey[ask.level]) : "" })}
          verb={t("bp_btn")}
          danger
          onNo={() => setAsk(null)}
          onYes={async () => {
            await api.setLocalBypass(ask.name, true);
            setAsk(null);
            await load();
          }}
        />
      )}
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
