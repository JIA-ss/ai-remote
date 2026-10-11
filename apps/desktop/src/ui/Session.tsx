// The live session: the remote screen is the subject. The capsule says who drives, the frame's
// colour repeats it, the turn line shows who had the controls along time, and the rail is a running
// document of steps with the one pending decision lifted above it.

import { useEffect, useMemo, useRef, useState } from "react";
import { api, type LiveView } from "../lib/api";
import { commands } from "../bindings";
import { demoScreenUrl } from "../lib/demo";
import { currentLang, t } from "../lib/i18n";
import type { Computer } from "../lib/store";
import type { Step } from "../lib/types";
import { createDecoder } from "../lib/video";
import { DecisionCard } from "./Decide";
import { RemoteInput, type ScreenRect } from "./RemoteInput";
import { Glyph, Pointer, SeatMark, type Seat } from "./glyphs";
import { Icon } from "./Icon";
import { computerFacts, Facts, NavRail, TitleBar, type Nav } from "./Shell";
import { TurnLine, turnSpec } from "./TurnLine";
import { agentLabel, clock, countdown, cx, useNow, useSize } from "./util";

type Bar = "agent" | "waiting" | "you" | "paused" | "idle" | "offline" | "connecting";

function barOf(c: Computer, hasFrame: boolean): Bar {
  if (c.online === false) return "offline";
  if (c.feed.pending.length) return "waiting";
  if (c.feed.driver.state === "person") return "you";
  if (c.feed.driver.state === "paused") return "paused";
  if (!hasFrame && api.kind === "tauri") return "connecting";
  return Object.keys(c.feed.sessions).some((id) => !c.feed.sessions[id].person) ? "agent" : "idle";
}

const SEAT: Record<Bar, Seat> = {
  agent: "agent",
  waiting: "waiting",
  you: "you",
  paused: "paused",
  idle: "idle",
  offline: "idle",
  connecting: "agent",
};

function Capsule({
  bar,
  agent,
  host,
  device,
  tool,
  path,
  mayWatch,
  busy,
  onAction,
}: {
  bar: Bar;
  agent: string;
  host: string;
  device: string;
  /** The tool a pending approval is for. */
  tool: string;
  path: "direct" | "relay" | null;
  mayWatch: boolean;
  busy: boolean;
  onAction: (a: "take_over" | "pause" | "hand_back" | "resume") => void;
}) {
  const lines: Record<Bar, [string, string]> = {
    agent: [t("cap_agent_1", { agent }), t("cap_agent_2", { device })],
    waiting: [t("cap_wait_1", { agent }), t("cap_wait_2", { tool })],
    you: [t("cap_you_1"), t(mayWatch ? "cap_you_2w" : "cap_you_2", { agent })],
    paused: [t("cap_paused_1"), t("cap_paused_2", { agent })],
    idle: [t("cap_idle_1", { host }), t("cap_idle_2", { device })],
    offline: [t("cap_off_1", { host }), t("cap_off_2")],
    connecting: [t("cap_conn_1", { host }), t("cap_conn_2")],
  };
  const take = (
    <button className="pill take" disabled={busy} onClick={() => onAction("take_over")}>
      {t("take_over")}
    </button>
  );
  const pause = (
    <button className="pill" disabled={busy} onClick={() => onAction("pause")}>
      <Glyph name="paused" size={16} />
      {t("pause_agent")}
    </button>
  );
  const actions: Record<Bar, React.ReactNode> = {
    agent: (
      <>
        {take}
        {pause}
      </>
    ),
    waiting: take,
    idle: take,
    you: (
      <button className="pill ink" disabled={busy} onClick={() => onAction("hand_back")}>
        {t("hand_back")}
        <span className="kbd">Ctrl+Alt+H</span>
      </button>
    ),
    paused: (
      <button className="pill ink" disabled={busy} onClick={() => onAction("resume")}>
        <Icon name="play" size={14} />
        {t("resume")}
      </button>
    ),
    offline: null,
    connecting: pause,
  };
  return (
    <div className="capsule" style={{ left: "50%", transform: "translateX(-50%)", top: 24 }}>
      <span className="disc">
        <SeatMark state={SEAT[bar]} h={20} />
      </span>
      <div className="who">
        <span className={cx("l1", bar === "you" && "you")}>{lines[bar][0]}</span>
        <span className="l2">{lines[bar][1]}</span>
      </div>
      {actions[bar]}
      {path && (
        <>
          <span className="vr" />
          <span className="q">
            <Glyph name={path === "relay" ? "relayed" : "direct"} size={16} />
            {t(path === "relay" ? "relayed" : "direct")}
          </span>
        </>
      )}
    </div>
  );
}

/** The remote screen: decoded video in the app, a still picture in the demo. */
export function Stream({
  host,
  onFrame,
  onSize,
  maxFps,
  maxLongEdge,
  onScreen,
}: {
  host: string;
  onFrame?: () => void;
  onSize?: (w: number, h: number) => void;
  /** Where the streamed monitor is in screen coordinates (for your input while you drive). */
  onScreen?: (r: ScreenRect | null) => void;
  /** A small preview asks for less: fewer frames, a smaller picture. */
  maxFps?: number;
  maxLongEdge?: number;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    if (api.kind !== "tauri" || !canvas.current) return;
    let view: LiveView | null = null;
    let stopped = false;
    let first = true;
    const decoder = createDecoder(
      canvas.current,
      () => view?.requestKeyframe(),
      (w, h) => {
        onSize?.(w, h);
        if (first) {
          first = false;
          onFrame?.();
        }
      },
    );
    api
      .watchScreen(host, (f) => decoder.push(f), { maxFps, maxLongEdge })
      .then((v) => {
        if (stopped) v?.stop();
        else {
          view = v;
          onScreen?.(v?.screen ?? null);
        }
      })
      .catch((e) => void commands.logFromPage("error", `Cannot start live video: ${String(e)}`));
    return () => {
      stopped = true;
      view?.stop();
      decoder.close();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host]);
  if (api.kind !== "tauri") {
    return <img className="rd-holder" src={demoScreenUrl()} alt="" style={{ width: "100%", height: "100%", objectFit: "cover" }} onLoad={() => { onSize?.(1600, 1000); onFrame?.(); onScreen?.({ x: 0, y: 0, width: 1600, height: 1000 }); }} />;
  }
  return <canvas ref={canvas} className="rd-holder" style={{ width: "100%", height: "100%" }} />;
}

/** One row of the steps log. `cls` follows the design's grammar: done | you | sys, the rule above
 *  (u-agent, u-you, u-wait, u-waited, u-sys) and below (d-...) the node, and the node (n-...). */
function Row({ cls, time, text, sub, mono }: { cls: string; time: string; text: React.ReactNode; sub?: string | null; mono?: boolean }) {
  return (
    <div className={cx("lr", cls)}>
      <span className="t">{time}</span>
      <span className="r">{/n-/.test(cls) && <i />}</span>
      <div className="x">
        {text}
        {sub && <span className={cx("sub", mono && "mono")}>{sub}</span>}
      </div>
    </div>
  );
}

type LogItem = { ts: number; key: string; node: React.ReactNode };

function logItems(c: Computer, agent: string, host: string): LogItem[] {
  const items: LogItem[] = [];
  const stepText = (s: Step) => s.intent ?? s.tool;
  for (const s of c.feed.steps) {
    const who = s.byPerson ? "you u-you d-you n-you" : "done u-agent d-agent";
    if (s.state === "waiting") {
      items.push({
        ts: s.startedMs,
        key: s.key,
        node: <Row cls="done from-card u-wait d-agent n-waithead" time={clock(s.startedMs)} text={t("s_asked", { tool: s.tool })} />,
      });
      continue;
    }
    if (s.approvalId) {
      const by = s.answeredBy;
      const allowed = s.decision === "approved";
      const answer =
        s.decision === "timeout" || s.decision === "unavailable"
          ? t("s_timeout")
          : by === "host"
            ? t(allowed ? "s_allowed_host" : "s_denied_host", { host })
            : t(allowed ? "s_allowed" : "s_denied", { device: by ?? "" });
      items.push({
        ts: s.startedMs,
        key: `${s.key}/asked`,
        node: <Row cls="done u-waited d-agent n-wait" time={clock(s.startedMs)} text={t("s_asked", { tool: s.tool })} sub={stepText(s) !== s.tool ? stepText(s) : null} />,
      });
      items.push({
        ts: s.decidedMs ?? s.startedMs + 1,
        key: `${s.key}/answer`,
        node: <Row cls="you u-agent d-waited n-you" time={clock(s.decidedMs ?? s.startedMs)} text={answer} sub={s.note ? `“${s.note}”` : null} />,
      });
      if (allowed && s.state !== "refused") {
        items.push({
          ts: (s.decidedMs ?? s.startedMs) + 2,
          key: `${s.key}/ran`,
          node: <Row cls={who} time={clock(s.decidedMs ?? s.startedMs)} text={stepText(s)} sub={s.error ?? null} />,
        });
      }
      continue;
    }
    if (s.state === "refused") {
      const why = s.decision === "turn" ? t("s_refused_turn") : t("s_refused");
      items.push({ ts: s.startedMs, key: s.key, node: <Row cls="sys u-agent d-agent n-sys" time={clock(s.startedMs)} text={why} sub={s.tool} mono /> });
      continue;
    }
    items.push({
      ts: s.startedMs,
      key: s.key,
      node: (
        <Row
          cls={who}
          time={clock(s.startedMs)}
          text={stepText(s)}
          sub={s.state === "failed" ? (s.error ?? t("s_failed")) : s.intent ? s.tool : null}
          mono={s.state !== "failed" && !!s.intent}
        />
      ),
    });
  }
  for (const turn of c.feed.turns) {
    const d = turn.driver;
    let text: string;
    let cls = "you u-agent d-agent n-you";
    if (turn.message) {
      text = t("s_note");
    } else if (d?.state === "person") {
      text = t("s_took_over", { device: turn.by });
      cls = "you u-agent d-you n-you";
    } else if (d?.state === "paused") {
      text = t("s_paused", { device: turn.by });
      cls = "you u-agent d-sys n-you";
    } else {
      text = turn.note !== undefined || c.feed.turns.some((x) => x.driver?.state === "person") ? t("s_handed") : t("s_resumed");
      cls = "you u-you d-agent n-you";
    }
    items.push({
      ts: turn.ts,
      key: `turn/${turn.ts}/${turn.by}`,
      node: <Row cls={cls} time={clock(turn.ts)} text={text} sub={turn.message ? `“${turn.message}”` : turn.note ? `“${turn.note}”` : null} />,
    });
  }
  void agent;
  return items.sort((a, b) => b.ts - a.ts);
}

export function SessionPage({
  c,
  device,
  waiting,
  onBack,
  onNav,
}: {
  c: Computer;
  device: string;
  waiting: number;
  onBack: () => void;
  onNav: (n: Nav | "home") => void;
}) {
  const now = useNow(1000);
  const zh = currentLang() === "zh";
  const host = c.host.name;
  const agent = agentLabel(c.feed.agent);
  const [stageRef, stage] = useSize<HTMLDivElement>();
  const [aspect, setAspect] = useState(16 / 10);
  const [hasFrame, setHasFrame] = useState(false);
  const [screen, setScreen] = useState<ScreenRect | null>(null);
  const [tab, setTab] = useState<"steps" | "sees">("steps");
  const [message, setMessage] = useState("");
  const [sent, setSent] = useState(false);
  const [handNote, setHandNote] = useState("");
  const [mayWatch, setMayWatch] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [acting, setActing] = useState(false);
  const cardRef = useRef<HTMLDivElement>(null);

  const bar = barOf(c, hasFrame);
  const pending = c.feed.pending[0];
  const askedStep = pending ? c.feed.steps.find((s) => s.approvalId === pending.id) : undefined;
  const drive = c.feed.driver.state === "person" ? "you" : bar === "agent" || bar === "waiting" ? "agent" : "idle";

  const turn = useMemo(() => turnSpec(c.feed, now, c.online !== false), [c.feed, now, c.online]);
  const items = useMemo(() => logItems(c, agent, host), [c, agent, host]);
  const running = [...c.feed.steps].reverse().find((s) => s.state === "running" && !s.byPerson);

  // Geometry from the design: the capsule band on top, the picture in the middle (kept to the
  // remote's aspect), the turn line and one floor line below.
  const inset = 12;
  const top = 24 + 52 + 16;
  const below = 20 + 24 + 16 + 40 + 16;
  let sw = Math.max(stage.w - inset * 2, 0);
  let sh = Math.round(sw / aspect);
  const maxH = Math.max(stage.h - top - below, 120);
  if (sh > maxH) {
    sh = maxH;
    sw = Math.round(sh * aspect);
  }
  const left = Math.round((stage.w - sw) / 2);
  const lineTop = top + sh + 20;
  const floorTop = Math.round(lineTop + 24 + (stage.h - (lineTop + 24) - 40) / 2);

  const act = async (a: "take_over" | "pause" | "hand_back" | "resume") => {
    if (acting) return;
    setActing(true);
    setActionError(null);
    try {
      if (a === "take_over") await api.setTurn(host, "take_over", { agentMayWatch: mayWatch });
      else if (a === "hand_back") {
        await api.setTurn(host, "hand_back", { note: handNote.trim() || undefined });
        setHandNote("");
      } else await api.setTurn(host, a);
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    } finally {
      setActing(false);
    }
  };

  const send = async () => {
    const text = message.trim();
    if (!text) return;
    try {
      setActionError(null);
      await api.tellAgent(host, text);
      setMessage("");
      setSent(true);
      setTimeout(() => setSent(false), 4000);
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    }
  };

  const composerPh = pending ? t("composer_decide") : bar === "paused" ? t("composer_decide") : t("composer_ph");
  const composerHint = sent
    ? t("composer_sent")
    : pending
      ? t("composer_after")
      : bar === "paused"
        ? t("composer_resume")
        : t("composer_hint_short");

  return (
    <div className="win" style={{ width: "100vw", height: "100vh" }}>
      <TitleBar title={host} facts={<Facts items={computerFacts(c)} />} onBack={onBack} />
      <div className="body-row">
        <NavRail waiting={waiting} os={c.os ?? "windows"} onNav={onNav} />
        <div className="stage-wrap" style={{ flex: 1, minWidth: 0, marginLeft: 8, marginBottom: 8 }}>
          <section className="stagebox stage" ref={stageRef} style={{ width: "100%", height: "100%" }}>
            <Capsule
              bar={bar}
              agent={agent}
              host={host}
              device={device}
              tool={pending?.tool ?? ""}
              path={c.online === true ? c.host.path === "relay" ? "relay" : c.host.path === "direct" ? "direct" : null : null}
              mayWatch={c.feed.driver.state === "person" && c.feed.driver.agent_may_watch}
              busy={acting}
              onAction={act}
            />
            {actionError && <div className="session-error" role="alert">{actionError}</div>}
            <div className={cx("frame", drive)} style={{ left, top, width: sw, height: sh }} />
            <div className={cx("stream", bar === "offline" && "desat")} style={{ left, top, width: sw, height: sh }}>
              {c.online !== false && (
                <Stream host={host} onFrame={() => setHasFrame(true)} onSize={(w, h) => setAspect(w / h)} onScreen={setScreen} />
              )}
              {bar === "you" && (
                <RemoteInput host={host} screen={screen} mac={c.os === "mac"} onHandBack={() => void act("hand_back")} />
              )}
            </div>
            {turn && (
              <div className="turnline" style={{ left, width: sw, top: lineTop }}>
                <span className="tnum" style={{ width: 40 }}>
                  {clock(Math.min(...c.feed.steps.map((s) => s.startedMs), now), false)}
                </span>
                <TurnLine spec={turn} w={Math.max(sw - 40 - 14 - (c.online !== false ? 70 : 0) - 14, 0)} />
                {c.online !== false && (
                  <span className="live">
                    <i />
                    {t("live")}
                  </span>
                )}
              </div>
            )}
            {pending ? (
              <div className="floorcap wait" style={{ top: floorTop, left: "50%", transform: "translateX(-50%)" }}>
                <span className="w">
                  <Pointer who="you" h={13} ui onDark />
                  {t("waiting_for_you")}
                </span>
                <span className="m">{t("banner_wait", { agent, tool: pending.tool })}</span>
                <span className="t">{t("denied_in", { time: countdown(pending.expiresMs, now, zh) })}</span>
                <button className="b" onClick={() => cardRef.current?.scrollIntoView({ behavior: "smooth", block: "center" })}>
                  {t("review")}
                </button>
              </div>
            ) : (
              <div className="stage-note" style={{ top: floorTop + 10, left: "50%", transform: "translateX(-50%)" }}>
                <Icon name={bar === "you" ? "keyboard" : bar === "paused" ? "pause" : "eye"} size={14} />
                <span>
                  {bar === "you"
                    ? t("st_driving", { host })
                    : bar === "paused"
                      ? t("st_paused_note", { agent })
                      : bar === "idle"
                        ? t("st_nobody", { host })
                        : t("st_watching")}
                </span>
              </div>
            )}
            {bar === "connecting" && (
              <div className="scard" style={{ left: "50%", top: top + sh / 2, transform: "translate(-50%, -50%)" }}>
                <div className="hd2">{t("conn_title", { host })}</div>
                <div className="bd">{t("conn_body")}</div>
              </div>
            )}
            {bar === "offline" && (
              <div className="scard" style={{ left: "50%", top: top + sh / 2, transform: "translate(-50%, -50%)" }}>
                <div className="row" style={{ gap: 14, alignItems: "flex-start" }}>
                  <div>
                    <div className="hd2">{t("off_title", { host })}</div>
                    <div className="bd">{c.problem ?? t("off_body")}</div>
                  </div>
                </div>
                <div className="acts">
                  <button className="btn" onClick={onBack}>
                    {t("back_computers")}
                  </button>
                </div>
              </div>
            )}
          </section>
        </div>
        <aside className="rail session-rail" style={{ marginLeft: 24, marginRight: 16, marginBottom: 8 }}>
          <div className="seg" role="tablist">
            <span className={cx(tab === "steps" && "sel")} onClick={() => setTab("steps")} role="tab">
              {t("tab_steps")}
            </span>
            <span className={cx(tab === "sees" && "sel")} onClick={() => setTab("sees")} role="tab">
              {t("tab_sees")}
            </span>
          </div>
          <div className={cx("rail-scroll", tab === "sees" && "nofade")} style={{ marginTop: 24, overflowY: "auto" }}>
            {tab === "sees" ? (
              <div className="note-box">
                <Icon name="info" size={14} />
                <span>{t("sees_soon")}</span>
              </div>
            ) : (
              <>
                {c.feed.task && (
                  <div className="lr head">
                    <span className="t">
                      <b>{t("task_l")}</b>
                      {clock(c.feed.task.ts, false)}
                    </span>
                    <span className="r" />
                    <div className="x">
                      {c.feed.task.text}
                      <span className="sub">{t("task_from", { agent, device: c.feed.task.peer })}</span>
                    </div>
                  </div>
                )}
                {pending && (
                  <div ref={cardRef} style={{ marginTop: c.feed.task ? 24 : 0 }}>
                    <DecisionCard p={pending} host={host} agent={agent} askedMs={askedStep?.startedMs} />
                  </div>
                )}
                <div className="log" style={{ marginTop: pending ? 20 : c.feed.task ? 28 : 0 }}>
                  {running && (
                    <div className="lr now d-agent n-live">
                      <span className="t">
                        <b className="agent">{t("now")}</b>
                        {clock(running.startedMs)}
                      </span>
                      <span className="r">
                        <i />
                      </span>
                      <div className="x">
                        <div className="now-hd hd">{running.intent ?? running.tool}</div>
                        {running.intent && <div className="now-sub">{running.tool}</div>}
                      </div>
                    </div>
                  )}
                  {items.filter((i) => !running || !i.key.startsWith(running.key)).map((i) => (
                    <div key={i.key}>{i.node}</div>
                  ))}
                  {items.length === 0 && !running && (
                    <div className="note-box">
                      <Icon name="info" size={14} />
                      <span>{t("no_steps")}</span>
                    </div>
                  )}
                </div>
              </>
            )}
          </div>
          {tab === "steps" && (
            <div className="composer-foot">
              {bar === "you" ? (
                <div className="hb">
                  <div className="tg">
                    <span className="grow">{t("let_watch")}</span>
                    <button
                      className={cx("switch", mayWatch && "on")}
                      aria-pressed={mayWatch}
                      onClick={() => {
                        const next = !mayWatch;
                        setMayWatch(next);
                        void api.setTurn(host, "take_over", { agentMayWatch: next });
                      }}
                    />
                  </div>
                  <div className="lbl">{t("what_changed")}</div>
                  <textarea className="ta ta-input" rows={2} value={handNote} onChange={(e) => setHandNote(e.target.value)} />
                  <div className="ft">{t("optional_sent")}</div>
                  <div className="ft" style={{ marginTop: 4 }}>
                    {t("handback_note")}
                  </div>
                </div>
              ) : (
                <div className={cx("composer", bar === "offline" && "off")}>
                  <div className="tx">
                    {bar === "offline" ? (
                      <span className="ph">{t("composer_offline", { host })}</span>
                    ) : (
                      <>
                        <input
                          className="ph-input"
                          value={message}
                          placeholder={composerPh}
                          onChange={(e) => setMessage(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") void send();
                          }}
                        />
                        <span className="hint">{composerHint}</span>
                      </>
                    )}
                  </div>
                  {bar !== "offline" && (
                    <button className="send" onClick={() => void send()} aria-label={t("send")} disabled={!message.trim()}>
                      <Icon name="send" size={16} />
                    </button>
                  )}
                </div>
              )}
            </div>
          )}
        </aside>
      </div>
    </div>
  );
}
