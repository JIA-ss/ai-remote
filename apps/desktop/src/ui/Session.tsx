// The live session: the remote screen is the subject, edge to edge on a black stage, never scaled
// past 1:1 device pixels. A floating bar says who drives and holds the actions, a corner HUD shows
// what arrives, the rail is a running document of steps with the one pending decision lifted above
// it. Full screen (⌃⌘F) keeps only the stage; the bars come back on movement and leave after 3 s.

import { useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { api, type LiveView, type PointerPosition } from "../lib/api";
import { commands } from "../bindings";
import { demoScreenUrl } from "../lib/demo";
import { currentLang, t } from "../lib/i18n";
import { loadStreamQuality, saveStreamQuality, STREAM_QUALITIES, streamQuality } from "../lib/stream-quality";
import type { Computer } from "../lib/store";
import type { Step } from "../lib/types";
import { createHostClock, startPinging } from "../lib/clock";
import { isFullscreen, setFullscreen, watchFullscreen } from "../lib/fullscreen";
import { preferredCodecs } from "../lib/codec";
import { createPointerLayer, type PointerLayer } from "../lib/pointer";
import { createRateController } from "../lib/rate";
import { createDecoder, type VideoStats } from "../lib/video";
import { DecisionCard } from "./Decide";
import { RemoteInput, type ScreenRect } from "./RemoteInput";
import { Glyph, Pointer, SeatMark, type Seat } from "./glyphs";
import { Icon } from "./Icon";
import { computerFacts, Facts, TitleBar, type Nav } from "./Shell";
import { TurnLine, turnSpec } from "./TurnLine";
import { agentLabel, clock, countdown, cx, relayNote, useNow, useSize } from "./util";

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

/** The remote screen: decoded video in the app, a still picture in the demo. */
export function Stream({
  host,
  onFrame,
  onSize,
  maxFps,
  maxLongEdge,
  bitrateKbps,
  onScreen,
  onStats,
  onRestart,
  onError,
}: {
  host: string;
  onFrame?: () => void;
  onSize?: (w: number, h: number) => void;
  /** Where the streamed monitor is in screen coordinates (for your input while you drive). */
  onScreen?: (r: ScreenRect | null) => void;
  onStats?: (stats: VideoStats | null) => void;
  /** Clears the previous frame and geometry while the new stream is starting. */
  onRestart?: () => void;
  onError?: (reason: string | null) => void;
  /** A small preview asks for less: fewer frames, a smaller picture. */
  maxFps?: number;
  maxLongEdge?: number;
  bitrateKbps?: number;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const pointerCanvas = useRef<HTMLCanvasElement>(null);
  const [ended, setEnded] = useState<string | null>(null);
  const [retryGeneration, setRetryGeneration] = useState(0);
  const callbacks = useRef({ onFrame, onSize, onScreen, onStats, onRestart, onError });
  callbacks.current = { onFrame, onSize, onScreen, onStats, onRestart, onError };
  useEffect(() => {
    callbacks.current.onRestart?.();
    callbacks.current.onScreen?.(null);
    callbacks.current.onStats?.(null);
    if (canvas.current) {
      canvas.current.width = 0;
      canvas.current.height = 0;
    }
    if (api.kind !== "tauri" || !canvas.current) return;
    setEnded(null);
    callbacks.current.onError?.(null);
    let view: LiveView | null = null;
    let stopped = false;
    let first = true;
    const clock = createHostClock();
    let stopPinging = () => {};
    // The host's pointer, when it sends it separately; positions may come before the start reply.
    let pointer: PointerLayer | null = null;
    let pointerAt: PointerPosition | null = null;
    const pointerLayer = () =>
      (pointer ??= pointerCanvas.current && canvas.current ? createPointerLayer(pointerCanvas.current, canvas.current) : null);
    const movePointer = () => {
      if (!stopped && pointerAt && view?.screen) pointerLayer()?.move(pointerAt, view.screen);
    };
    // A per-second line of receiver metrics, for measurement runs (tools/stream-e2e.py sets it).
    const trace = localStorage.getItem("ai-remote.video-trace") === "1";
    const started = performance.now();
    // The api's default when no quality was chosen.
    const requestedKbps = bitrateKbps ?? 6000;
    const rate = createRateController(requestedKbps, (kbps, why) => {
      view?.setBitrate(kbps);
      void commands.logFromPage("info", `Live video: bitrate ${kbps} kbit/s (${why}; requested ${requestedKbps})`);
    });
    const decoder = createDecoder(
      canvas.current,
      () => view?.requestKeyframe(),
      (w, h) => {
        if (stopped) return;
        callbacks.current.onSize?.(w, h);
        if (first) {
          first = false;
          callbacks.current.onFrame?.();
        }
      },
      (stats) => {
        const path = clock.errorMs();
        if (path !== null) rate.pathDelay(path);
        const adapted = rate.current();
        if (trace) {
          void commands.logFromPage("info", `Live video: stats ${JSON.stringify({ t: Math.round(performance.now() - started),
            fps: +stats.decodedFps.toFixed(1), kbps: Math.round(stats.receivedKbps),
            latency: stats.latencyMs === null ? null : Math.round(stats.latencyMs), target: adapted })}`);
        }
        if (!stopped) callbacks.current.onStats?.(adapted < requestedKbps ? { ...stats, adaptedKbps: adapted } : stats);
      },
      clock,
      (latencyMs, now) => rate.observe(latencyMs, now),
    );
    const end = (reason: string) => {
      if (stopped) return;
      stopped = true;
      stopPinging();
      pointer?.hide();
      view?.stop();
      decoder.close();
      if (canvas.current) {
        canvas.current.width = 0;
        canvas.current.height = 0;
      }
      callbacks.current.onRestart?.();
      callbacks.current.onScreen?.(null);
      callbacks.current.onStats?.(null);
      const message = reason || t("stream_ended_reason");
      setEnded(message);
      callbacks.current.onError?.(message);
    };
    api
      .watchScreen(host, (f) => {
        if (stopped) return;
        rate.received(f.byteLength, performance.now());
        decoder.push(f);
      },
        { maxFps, maxLongEdge, bitrateKbps, codecs: preferredCodecs(), onPong: clock.pong, onEnded: end,
          onPointer: (at) => { pointerAt = at; movePointer(); },
          onPointerShape: (shape) => {
            if (stopped) return;
            pointerLayer()?.shape(shape);
            if (trace) void commands.logFromPage("info", `Live video: pointer shape ${shape.id} ${shape.width}x${shape.height}`);
          } })
      .then((v) => {
        if (stopped) v?.stop();
        else {
          view = v;
          callbacks.current.onScreen?.(v?.screen ?? null);
          if (v) stopPinging = startPinging(clock, (n) => v.ping(n));
          movePointer();
        }
      })
      .catch((e) => {
        if (stopped) return;
        void commands.logFromPage("error", `Cannot start live video: ${String(e)}`);
        end(e instanceof Error ? e.message : String(e));
      });
    return () => {
      stopped = true;
      stopPinging();
      pointer?.hide();
      view?.stop();
      decoder.close();
      callbacks.current.onScreen?.(null);
      callbacks.current.onStats?.(null);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host, maxFps, maxLongEdge, bitrateKbps, retryGeneration]);
  if (api.kind !== "tauri") {
    return <img key={`${host}/${maxFps}/${maxLongEdge}/${bitrateKbps}/${retryGeneration}`} className="rd-holder" src={demoScreenUrl()} alt="" style={{ width: "100%", height: "100%", objectFit: "cover" }} onLoad={() => { callbacks.current.onSize?.(1600, 1000); callbacks.current.onFrame?.(); callbacks.current.onScreen?.({ x: 0, y: 0, width: 1600, height: 1000 }); }} />;
  }
  return (
    <>
      <canvas ref={canvas} className="rd-holder" style={{ width: "100%", height: "100%" }} />
      <canvas ref={pointerCanvas} className="remote-pointer" aria-hidden="true" />
      {ended && (
        <div className="stream-ended" role="alert">
          <Icon name="warn" size={24} />
          <strong>{t("stream_ended_title")}</strong>
          <p>{ended}</p>
          <button type="button" className="btn" onClick={() => {
            setEnded(null);
            callbacks.current.onError?.(null);
            callbacks.current.onRestart?.();
            setRetryGeneration((generation) => generation + 1);
          }}><Icon name="refresh" size={14} />{t("stream_retry")}</button>
        </div>
      )}
    </>
  );
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

/** The floating bar over the picture: who drives, the actions, the route. Dark in both themes. */
function DriverBar({
  bar,
  agent,
  host,
  device,
  tool,
  path,
  pathTitle,
  mayWatch,
  busy,
  onAction,
  immersive,
  onLeave,
}: {
  bar: Bar;
  agent: string;
  host: string;
  device: string;
  /** The tool a pending approval is for. */
  tool: string;
  path: "direct" | "relay" | null;
  /** On hover: why the connection is relayed. */
  pathTitle?: string;
  mayWatch: boolean;
  busy: boolean;
  onAction: (a: "take_over" | "pause" | "hand_back" | "resume") => void;
  immersive: boolean;
  onLeave: () => void;
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
    <button className="nt-tool take" disabled={busy} onClick={() => onAction("take_over")}>
      {t("take_over")}
    </button>
  );
  const pause = (
    <button className="nt-tool" disabled={busy} onClick={() => onAction("pause")}>
      <Glyph name="paused" size={14} />
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
      <button className="nt-tool ink" disabled={busy} onClick={() => onAction("hand_back")}>
        {t("hand_back")}
        <span className="kbd">Ctrl+Alt+H</span>
      </button>
    ),
    paused: (
      <button className="nt-tool ink" disabled={busy} onClick={() => onAction("resume")}>
        <Icon name="play" size={14} />
        {t("resume")}
      </button>
    ),
    offline: null,
    connecting: pause,
  };
  return (
    <div className="nt-bar bottom">
      <span className="disc">
        <SeatMark state={SEAT[bar]} h={16} />
      </span>
      <div className="who">
        <span className={cx("l1", bar === "you" && "you")}>{lines[bar][0]}</span>
        <span className="l2">{lines[bar][1]}</span>
      </div>
      {actions[bar]}
      {path && (
        <>
          <span className="vr" />
          <span className={cx("nt-route", path === "relay" && "relay")} title={pathTitle}>
            <Glyph name={path === "relay" ? "relayed" : "direct"} size={14} />
            {t(path === "relay" ? "relayed" : "direct")}
          </span>
        </>
      )}
      {immersive && (
        <>
          <span className="vr" />
          <button className="nt-iconbtn" onClick={onLeave} aria-label={t("fs_exit")} title={`${t("fs_exit")} · ⌃⌘F`}>
            <Icon name="x" size={14} />
          </button>
        </>
      )}
    </div>
  );
}

/** Receiver numbers in the corner: what arrives, how late, over which route. */
function Hud({ stats, hasFrame, offline, demo, relay }: { stats: VideoStats | null; hasFrame: boolean; offline: boolean; demo: boolean; relay: boolean }) {
  const detail = stats
    ? t("stream_metrics_detail", {
        dropped: stats.droppedFrames,
        backlog: stats.drops.backlog,
        awaiting: stats.drops.awaitingKeyframe,
        errors: stats.drops.error + stats.drops.invalid,
        queue: stats.decodeQueueSize,
      })
    : undefined;
  let body: React.ReactNode;
  if (demo) body = <span className="dim">{t("stream_preview_metrics")}</span>;
  else if (offline) body = <span className="dim">{t("st_offline")}</span>;
  else if (hasFrame && stats)
    body = (
      <>
        <span>
          <b>{stats.width}×{stats.height}</b>
        </span>
        <span>
          <b>{stats.decodedFps.toFixed(0)}</b>
          <i className="u">{t("hud_fps")}</i>
        </span>
        <span>
          <b>{(stats.receivedKbps / 1000).toFixed(1)}</b>
          <i className="u">{t("hud_mbps")}</i>
        </span>
        {stats.latencyMs !== null && (
          <span title={t("stream_latency", { ms: Math.round(stats.latencyMs), error: (stats.latencyErrorMs ?? 0).toFixed(1) })}>
            <b>{Math.round(stats.latencyMs)}</b>
            <i className="u">ms</i>
          </span>
        )}
        {stats.adaptedKbps !== undefined && <span className="dim">{t("hud_lowered", { mbps: (stats.adaptedKbps / 1000).toFixed(1) })}</span>}
        <span className={cx(relay && "relay")}>{t(relay ? "relayed" : "direct")}</span>
      </>
    );
  else body = <span className="dim">{t("hud_waiting")}</span>;
  return (
    <div className="nt-hud stream-metrics" title={detail}>
      {body}
    </div>
  );
}

/* Bars slide 8 px in and out over 180 ms; the anchors are centred with `x`, so `y` is free. */
const fade = {
  initial: { opacity: 0, y: 8, x: "-50%" },
  animate: { opacity: 1, y: 0, x: "-50%" },
  exit: { opacity: 0, y: 8, x: "-50%" },
  transition: { duration: 0.18, ease: [0.2, 0, 0, 1] as const },
};
/* The HUD sits in a corner, so it only fades. */
const hudFade = { initial: { opacity: 0 }, animate: { opacity: 1 }, exit: { opacity: 0 }, transition: { duration: 0.18 } };

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
  void waiting;
  void onNav;
  const now = useNow(1000);
  const zh = currentLang() === "zh";
  const host = c.host.name;
  const relay = relayNote(c.host);
  const agent = agentLabel(c.feed.agent);
  const [stageRef, stage] = useSize<HTMLDivElement>();
  const [lineRef, line] = useSize<HTMLDivElement>();
  const [native, setNative] = useState<{ w: number; h: number } | null>(null);
  const [hasFrame, setHasFrame] = useState(false);
  const [screen, setScreen] = useState<ScreenRect | null>(null);
  const [quality, setQuality] = useState(loadStreamQuality);
  const [stats, setStats] = useState<VideoStats | null>(null);
  const [tab, setTab] = useState<"steps" | "sees">("steps");
  const [message, setMessage] = useState("");
  const [sent, setSent] = useState(false);
  const [handNote, setHandNote] = useState("");
  const [mayWatch, setMayWatch] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [acting, setActing] = useState(false);
  const [immersive, setImmersive] = useState(false);
  // In full screen the bars show on movement and leave after 3 s of quiet.
  const [chrome, setChrome] = useState(true);
  const chromeTimer = useRef<number | null>(null);
  const cardRef = useRef<HTMLDivElement>(null);

  const resetVideo = () => {
    setHasFrame(false);
    setScreen(null);
    setStats(null);
    setNative(null);
  };

  useEffect(() => {
    if (c.online === false) resetVideo();
  }, [c.online]);

  const bar = barOf(c, hasFrame);
  const pending = c.feed.pending[0];
  const askedStep = pending ? c.feed.steps.find((s) => s.approvalId === pending.id) : undefined;
  const driving = bar === "you";

  const turn = useMemo(() => turnSpec(c.feed, now, c.online !== false), [c.feed, now, c.online]);
  const items = useMemo(() => logItems(c, agent, host), [c, agent, host]);
  const running = [...c.feed.steps].reverse().find((s) => s.state === "running" && !s.byPerson);

  // Full screen: ours (⌃⌘F, the button) and the window's (the green button) end in the same state.
  const enter = (on: boolean) => {
    setImmersive(on);
    setChrome(true);
    void setFullscreen(on).catch((e) => setActionError(e instanceof Error ? e.message : String(e)));
  };
  useEffect(() => {
    void isFullscreen().then((on) => on && setImmersive(true));
    return watchFullscreen((on) => {
      setImmersive(on);
      setChrome(true);
    });
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.metaKey && !e.altKey && e.key.toLowerCase() === "f") {
        e.preventDefault();
        enter(!immersive);
      } else if (e.key === "Escape" && immersive && !driving) {
        enter(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [immersive, driving]);
  // Leaving the page while full screen gives the window back.
  useEffect(() => () => void setFullscreen(false).catch(() => {}), []);
  const touch = (e?: React.MouseEvent) => {
    if (!immersive) return;
    // While you drive, only the edges bring the bars back (they cover remote pixels).
    if (driving && e) {
      const r = e.currentTarget.getBoundingClientRect();
      const y = e.clientY - r.top;
      if (y > 56 && y < r.height - 72) return;
    }
    setChrome(true);
    if (chromeTimer.current) window.clearTimeout(chromeTimer.current);
    chromeTimer.current = window.setTimeout(() => setChrome(false), 3000);
  };
  useEffect(() => {
    if (immersive) touch();
    else if (chromeTimer.current) window.clearTimeout(chromeTimer.current);
    return () => {
      if (chromeTimer.current) window.clearTimeout(chromeTimer.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [immersive]);
  const showBars = !immersive || chrome;

  // The picture: fills the stage, keeps the remote's aspect, never larger than 1:1 device pixels.
  const aspect = native ? native.w / native.h : 16 / 10;
  const dpr = window.devicePixelRatio || 1;
  let sw = stage.w;
  let sh = sw / aspect;
  if (sh > stage.h) {
    sh = stage.h;
    sw = sh * aspect;
  }
  if (native && sw > native.w / dpr) {
    sw = native.w / dpr;
    sh = native.h / dpr;
  }
  sw = Math.round(sw * dpr) / dpr;
  sh = Math.round(sh * dpr) / dpr;
  const left = Math.round(((stage.w - sw) / 2) * dpr) / dpr;
  const top = Math.round(((stage.h - sh) / 2) * dpr) / dpr;

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

  const qualityPicker = (
    <span className="nt-select-wrap">
      <select
        id="stream-quality"
        className="nt-select"
        aria-label={t("stream_quality")}
        value={quality.id}
        title={`${t("stream_requested_limits", { edge: quality.maxLongEdge, fps: quality.maxFps, mbps: quality.bitrateKbps / 1000 })}\n${t("stream_requested_note")}`}
        onChange={(e) => {
          const next = streamQuality(e.target.value);
          if (next.id === quality.id) return;
          resetVideo();
          saveStreamQuality(next.id);
          setQuality(next);
        }}
      >
        {STREAM_QUALITIES.map((profile) => <option key={profile.id} value={profile.id}>{t(profile.label)}</option>)}
      </select>
      <Icon name="chevD" size={12} />
    </span>
  );

  return (
    <div className={cx("nt-session", immersive && "immersive")}>
      <TitleBar
        title={host}
        facts={<Facts items={computerFacts(c)} />}
        onBack={onBack}
        end={
          <>
            {c.online === true && c.host.p2pObstacle === "firewall" && (
              <button
                className="btn sm"
                onClick={() =>
                  api.fixFirewall().catch((e) => setActionError(t("relay_fix_firewall_failed", { error: e instanceof Error ? e.message : String(e) })))
                }
              >
                {t("relay_fix_firewall")}
              </button>
            )}
            {qualityPicker}
            <button className="nt-iconbtn" onClick={() => enter(true)} aria-label={t("fs_enter")} title={`${t("fs_enter")} · ⌃⌘F`}>
              <Icon name="fullscreen" size={16} />
            </button>
          </>
        }
      />
      <div className="nt-session-body">
        <section
          className={cx("nt-stage", immersive && !chrome && driving && "hidecursor")}
          ref={stageRef}
          onMouseMove={touch}
          onMouseDown={() => immersive && !driving && touch()}
        >
          <div className={cx("nt-picture", bar === "offline" && "desat")} style={{ left, top, width: sw, height: sh }}>
            {c.online !== false && (
              <Stream
                host={host}
                maxFps={quality.maxFps}
                maxLongEdge={quality.maxLongEdge}
                bitrateKbps={quality.bitrateKbps}
                onFrame={() => setHasFrame(true)}
                onSize={(w, h) => setNative({ w, h })}
                onScreen={setScreen}
                onStats={setStats}
                onRestart={resetVideo}
                onError={setActionError}
              />
            )}
            {driving && hasFrame && screen !== null && (
              <RemoteInput host={host} screen={screen} mac={c.os === "mac"} onHandBack={() => void act("hand_back")} onError={setActionError} />
            )}
          </div>
          {actionError && <div className="session-error" role="alert">{actionError}</div>}
          <AnimatePresence initial={false}>
            {showBars && (
              <motion.div key="hud" {...hudFade} style={{ position: "absolute", inset: 0, pointerEvents: "none", zIndex: 7 }}>
                <Hud stats={stats} hasFrame={hasFrame} offline={c.online === false} demo={api.kind === "demo"} relay={c.host.path === "relay"} />
              </motion.div>
            )}
            {showBars && immersive && (
              <motion.div key="top" {...fade} className="nt-anchor top">
                <div className="nt-bar top">
                <span className="title">{host}</span>
                <span className="facts">
                  <Facts items={computerFacts(c)} />
                </span>
                <span className="vr" />
                {qualityPicker}
                <button className="nt-iconbtn" onClick={() => enter(false)} aria-label={t("fs_exit")} title={`${t("fs_exit")} · ⌃⌘F`}>
                  <Icon name="exitFullscreen" size={16} />
                </button>
                </div>
              </motion.div>
            )}
            {showBars && pending && (
              <motion.div key="wait" {...fade} className="nt-anchor" style={{ top: immersive ? 60 : 12 }}>
                <div className="nt-wait">
                <span className="w">
                  <Pointer who="you" h={12} ui onDark />
                  {t("waiting_for_you")}
                </span>
                <span className="m">{t("banner_wait", { agent, tool: pending.tool })}</span>
                <span className="t">{t("denied_in", { time: countdown(pending.expiresMs, now, zh) })}</span>
                <button
                  className="b"
                  onClick={() => {
                    if (immersive) enter(false);
                    setTimeout(() => cardRef.current?.scrollIntoView({ behavior: "smooth", block: "center" }), 50);
                  }}
                >
                  {t("review")}
                </button>
                </div>
              </motion.div>
            )}
            {showBars && (
              <motion.div key="bar" {...fade} className="nt-anchor bottom">
                    <DriverBar
                      bar={bar}
                      agent={agent}
                      host={host}
                      device={device}
                      tool={pending?.tool ?? ""}
                      path={c.online === true ? c.host.path === "relay" ? "relay" : c.host.path === "direct" ? "direct" : null : null}
                      pathTitle={relay?.detail}
                      mayWatch={c.feed.driver.state === "person" && c.feed.driver.agent_may_watch}
                      busy={acting}
                      onAction={act}
                      immersive={immersive}
                      onLeave={() => enter(false)}
                    />
              </motion.div>
            )}
          </AnimatePresence>
          {!pending && !immersive && hasFrame && (
            <div className="nt-note">
              <Icon name={driving ? "keyboard" : bar === "paused" ? "pause" : "eye"} size={13} />
              <span>
                {driving ? t("st_driving", { host }) : bar === "paused" ? t("st_paused_note", { agent }) : bar === "idle" ? t("st_nobody", { host }) : t("st_watching")}
              </span>
            </div>
          )}
          {bar === "connecting" && !actionError && (
            <div className="nt-stagecard">
              <div className="hd2">{t("conn_title", { host })}</div>
              <div className="bd">{t("conn_body")}</div>
            </div>
          )}
          {bar === "offline" && (
            <div className="nt-stagecard">
              <div className="hd2">{t("off_title", { host })}</div>
              <div className="bd">{c.problem ?? t("off_body")}</div>
              <div className="acts">
                <button className="btn" onClick={onBack}>
                  {t("back_computers")}
                </button>
              </div>
            </div>
          )}
        </section>
        {!immersive && (
          <aside className="nt-rail">
            {turn && (
              <div className="nt-line" ref={lineRef}>
                <span className="tnum" style={{ width: 36 }}>
                  {clock(Math.min(...c.feed.steps.map((s) => s.startedMs), now), false)}
                </span>
                <TurnLine spec={turn} w={Math.max(line.w - 36 - 10 - (c.online !== false ? 56 : 0) - 10, 0)} />
                {c.online !== false && (
                  <span className="live">
                    <i />
                    {t("live")}
                  </span>
                )}
              </div>
            )}
            <div className="seg" role="tablist">
              <span className={cx(tab === "steps" && "sel")} onClick={() => setTab("steps")} role="tab">
                {t("tab_steps")}
              </span>
              <span className={cx(tab === "sees" && "sel")} onClick={() => setTab("sees")} role="tab">
                {t("tab_sees")}
              </span>
            </div>
            <div className={cx("rail-scroll", tab === "sees" && "nofade")} style={{ overflowY: "auto" }}>
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
                {driving ? (
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
        )}
      </div>
    </div>
  );
}
