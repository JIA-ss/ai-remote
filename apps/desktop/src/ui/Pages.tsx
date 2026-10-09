// Main-window pages: a computer's overview, the approvals inbox, activity, pairing and settings.

import { useState } from "react";
import { api, type Paired } from "../lib/api";
import { currentLang, setLang, t } from "../lib/i18n";
import { allPending, groupOf, type Computer, type Snapshot } from "../lib/store";
import { DecisionCard } from "./Decide";
import { Stream } from "./Session";
import { Glyph, OsGlyph, Pointer, SeatMark, Wordmark } from "./glyphs";
import { Icon } from "./Icon";
import { computerFacts, Facts, statusOf } from "./Shell";
import { TurnLine, turnSpec } from "./TurnLine";
import { agentLabel, clock, cx, useNow, useSize } from "./util";

export function ComputerPage({ c, onWatch }: { c: Computer; onWatch: () => void }) {
  const now = useNow(1000);
  const st = statusOf(c, now);
  const agent = agentLabel(c.feed.agent);
  const pending = c.feed.pending[0];
  const askedStep = pending ? c.feed.steps.find((s) => s.approvalId === pending.id) : undefined;
  const steps = [...c.feed.steps].reverse().slice(0, 5);
  const [pvRef, pv] = useSize<HTMLDivElement>();
  const spec = turnSpec(c.feed, now, c.online !== false);
  const off = c.online === false;
  const group = groupOf(c, now);
  return (
    <div className="cpage">
      <div className="cp-top">
        <span className="tile">
          <OsGlyph os={c.os ?? "windows"} size={16} />
          <i className={cx("pd", off && "off")} />
        </span>
        <span className="nm">{c.host.name}</span>
        <span className="facts">
          <Facts items={computerFacts(c)} />
          <span>
            <i className={off ? "off" : "on"} />
            {off ? t("st_offline") : t("live")}
          </span>
        </span>
      </div>
      <div className="cp-hl">
        {group === "needs" || group === "you" ? <Pointer who="you" h={28} ui /> : group === "working" ? <Pointer who="agent" h={28} ui /> : <SeatMark state="idle" h={24} />}
        <div className="dsp">{st.text}</div>
      </div>
      {c.problem && <div className="cp-lede lede">{c.problem}</div>}
      <div className="cp-grid">
        {pending ? (
          <DecisionCard p={pending} host={c.host.name} agent={agent} askedMs={askedStep?.startedMs} wide />
        ) : (
          <div />
        )}
        <div className="pv-wrap">
          <div className="pv stage" ref={pvRef}>
            <div className={cx("shot", off && "desat")} style={{ height: Math.round(((pv.w || 400) - 24) * 10 / 16) }}>
              {!off && <Stream host={c.host.name} maxFps={2} maxLongEdge={960} />}
              <div className={cx("frame", c.feed.driver.state === "person" ? "you" : group === "working" || group === "needs" ? "agent" : "idle")} />
            </div>
            {spec && (
              <div className="pvline">
                <span className="tnum">{clock(Math.min(...c.feed.steps.map((s) => s.startedMs), now), false)}</span>
                <TurnLine spec={spec} w={Math.max((pv.w || 400) - 24 - 48, 0)} />
              </div>
            )}
            <div className="cap2">
              <SeatMark state={group === "needs" ? "waiting" : group === "you" ? "you" : group === "working" ? "agent" : "idle"} h={18} />
              <div style={{ display: "flex", flexDirection: "column", minWidth: 0 }}>
                <span className="l1">{st.text}</span>
                <span className="l2">{off ? t("st_offline") : t("live")}</span>
              </div>
              {!off && (
                <button className="watch" onClick={onWatch}>
                  <Icon name="eye" size={14} />
                  {t("watch_live")}
                </button>
              )}
            </div>
          </div>
        </div>
      </div>
      <div className="cp-cols">
        <div>
          <div className="colh">{t("recent_steps")}</div>
          <div className="log">
            {steps.length === 0 && <div className="body c3">{t("no_steps")}</div>}
            {steps.map((s) => (
              <div key={s.key} className={cx("lr", s.byPerson ? "you u-you d-you" : "done u-agent d-agent")}>
                <span className="t">{clock(s.startedMs)}</span>
                <span className="r" />
                <div className="x">
                  {s.intent ?? s.tool}
                  {s.intent && <span className="sub mono">{s.tool}</span>}
                </div>
              </div>
            ))}
          </div>
        </div>
        <div>
          <div className="colh">{t("this_computer")}</div>
          <dl className="dl">
            <dt>{t("connection")}</dt>
            <dd>{c.host.path ? t(c.host.path === "relay" ? "conn_relayed" : "conn_direct") : "—"}</dd>
            <dt>{t("agent_l")}</dt>
            <dd>{c.feed.agent ? agent : "—"}</dd>
            <dt>{t("device_key")}</dt>
            <dd className="mono">{c.host.device ? c.host.device.slice(0, 16) : "—"}</dd>
          </dl>
        </div>
        <div />
      </div>
    </div>
  );
}

export function ApprovalsPage({ snap, onOpen }: { snap: Snapshot; onOpen: (host: string) => void }) {
  const list = allPending(snap);
  return (
    <div className="cpage">
      <div className="cp-hl tab">
        <div className="hd">{t("approvals")}</div>
      </div>
      {list.length === 0 && (
        <div className="cp-lede body" style={{ marginLeft: 0 }}>
          —
        </div>
      )}
      <div style={{ display: "grid", gap: 24, maxWidth: 720, marginTop: 24 }}>
        {list.map((p) => {
          const c = snap.computers.find((x) => x.host.name === p.host)!;
          const asked = c.feed.steps.find((s) => s.approvalId === p.id);
          return (
            <div key={p.id} onDoubleClick={() => onOpen(p.host)}>
              <DecisionCard p={p} host={p.host} agent={agentLabel(c.feed.agent)} askedMs={asked?.startedMs} wide />
            </div>
          );
        })}
      </div>
    </div>
  );
}

export function ActivityPage({ snap }: { snap: Snapshot }) {
  const rows = snap.computers
    .flatMap((c) => c.feed.steps.map((s) => ({ s, host: c.host.name })))
    .sort((a, b) => b.s.startedMs - a.s.startedMs)
    .slice(0, 200);
  return (
    <div className="cpage">
      <div className="cp-hl tab">
        <div className="hd">{t("activity")}</div>
      </div>
      <div className="tbl" style={{ marginTop: 24 }}>
        {rows.map(({ s, host }) => (
          <div key={`${host}/${s.key}`} className="tr">
            <span className="t">{clock(s.startedMs)}</span>
            <span>
              <Pointer who={s.byPerson ? "you" : "agent"} h={12} ui />
            </span>
            <span className={cx("s", s.byPerson && "you")}>{s.intent ?? s.tool}</span>
            <span className="c">{host}</span>
            <span className="k">
              <span className="mono">{s.tool}</span>
            </span>
            <span className="o">{s.state}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

export function PairPage({ onPaired }: { onPaired: (host: string) => void }) {
  const [ticket, setTicket] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<Paired | null>(null);
  const pair = async () => {
    setBusy(true);
    setError(null);
    try {
      setDone(await api.pair(ticket.trim(), name.trim() || undefined));
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="cpage">
      <div className="cp-hl tab">
        <div className="hd">{t("pair")}</div>
      </div>
      {done ? (
        <div style={{ marginTop: 24, maxWidth: 560 }}>
          <div className="row" style={{ gap: 12 }}>
            <SeatMark state="both" h={28} />
            <div className="ttl">{done.peer}</div>
          </div>
          <div className="dsp safety" style={{ marginTop: 16 }}>
            {done.sas}
          </div>
          <div className="body c2" style={{ marginTop: 8 }}>
            {/* The host must show the same three words. */}
            {done.level && <Glyph name={done.level === "admin" ? "level3" : done.level === "control" ? "level2" : "level1"} size={14} />}
          </div>
          <div style={{ marginTop: 20 }}>
            <button className="btn primary lg" onClick={() => onPaired(done.peer)}>
              {done.peer}
            </button>
          </div>
        </div>
      ) : (
        <div style={{ marginTop: 24, maxWidth: 560, display: "grid", gap: 12 }}>
          <textarea className="ta-input" rows={4} value={ticket} onChange={(e) => setTicket(e.target.value)} placeholder="mcpmesh-invite:…" />
          <input className="field lg" value={name} onChange={(e) => setName(e.target.value)} placeholder="office-pc" />
          {error && (
            <div className="conseq">
              <Icon name="warn" size={16} />
              <span>{error}</span>
            </div>
          )}
          <div>
            <button className="btn primary lg" disabled={busy || !ticket.trim()} onClick={pair}>
              {t("pair")}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

export function SettingsPage() {
  const [lang, setL] = useState(currentLang());
  return (
    <div className="cpage">
      <div className="cp-hl tab">
        <div className="hd">{t("settings")}</div>
      </div>
      <div className="grp" style={{ marginTop: 24, maxWidth: 640 }}>
        <div className="gr noic">
          <div>
            <div className="tt">English / 中文</div>
          </div>
          <span className="end">
            <button
              className="btn"
              onClick={() => {
                const next = lang === "en" ? "zh" : "en";
                setLang(next);
                setL(next);
                location.reload();
              }}
            >
              {lang === "en" ? "中文" : "English"}
            </button>
          </span>
        </div>
      </div>
    </div>
  );
}

export function EmptyHome({ onPair }: { onPair: () => void }) {
  return (
    <div className="cpage" style={{ display: "grid", placeItems: "center", height: "100%" }}>
      <div style={{ textAlign: "center" }}>
        <Wordmark h={32} />
        <div className="body c2" style={{ marginTop: 16 }}>
          {t("no_computers_short")}
        </div>
        <button className="btn primary lg" style={{ marginTop: 20 }} onClick={onPair}>
          <Icon name="add" size={14} />
          {t("pair")}
        </button>
      </div>
    </div>
  );
}
