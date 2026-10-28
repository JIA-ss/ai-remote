// The live session: the remote screen is the subject, edge to edge on a black stage, never scaled
// past 1:1 device pixels. A floating bar says who drives and holds the actions, a corner HUD shows
// what arrives. LiveWorkspace places the complete workspace and host approvals beside this screen.
// Full screen (⌃⌘F) keeps only the stage; the bars come back on movement and leave after 3 s.
// While you drive, only the top and bottom edges, or the small tab under the bottom bar, bring them back.

import { useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { api, type LiveView, type PointerPosition } from "../lib/api";
import { commands } from "../bindings";
import { demoMonitors, demoScreenUrl } from "../lib/demo";
import { currentLang, t } from "../lib/i18n";
import { isMissingMonitor, loadMonitor, monitorNames, monitorOption, pickerOrder, primaryOf, saveMonitor, type RemoteMonitor } from "../lib/monitors";
import { loadStreamQuality, resolveQuality, saveStreamQuality, STREAM_QUALITIES, streamQuality } from "../lib/stream-quality";
import type { Computer } from "../lib/store";
import { createHostClock, startPinging } from "../lib/clock";
import { isFullscreen, setFullscreen, watchFullscreen } from "../lib/fullscreen";
import { preferredCodecs } from "../lib/codec";
import { createPointerLayer, type PointerLayer } from "../lib/pointer";
import { createRateController } from "../lib/rate";
import { createDecoder, type VideoStats } from "../lib/video";
import { RemoteInput, type ScreenRect } from "./RemoteInput";
import { watchFileDrop } from "../lib/drop";
import { Glyph, Pointer } from "./glyphs";
import { Icon } from "./Icon";
import { computerFacts, Facts, TitleBar, type Nav } from "./Shell";
import { agentLabel, countdown, cx, useNow, useSize } from "./util";

type Bar = "agent" | "waiting" | "you" | "paused" | "idle" | "offline" | "connecting";

function barOf(c: Computer, hasFrame: boolean): Bar {
  if (c.online === false) return "offline";
  if (c.feed.pending.length) return "waiting";
  if (c.feed.driver.state === "person") return "you";
  if (c.feed.driver.state === "paused") return "paused";
  if (!hasFrame && api.kind === "tauri") return "connecting";
  return Object.keys(c.feed.sessions).some((id) => !c.feed.sessions[id].person) ? "agent" : "idle";
}

/** The remote screen: decoded video in the app, a still picture in the demo. */
export function Stream({
  host,
  monitor = null,
  onMonitors,
  onMonitorMissing,
  onFrame,
  onSize,
  maxFps,
  maxLongEdge,
  bitrateKbps,
  onScreen,
  onStats,
  onRestart,
  onError,
  onPointerShown,
}: {
  host: string;
  /** The host's monitor to show (an index of its list); null: the primary one. */
  monitor?: number | null;
  /** The host's monitors, and the index of the one streamed (null from older hosts). */
  onMonitors?: (monitors: RemoteMonitor[], streamed: number | null) => void;
  /** The host refused `monitor` as one it does not have (unplugged since it was picked). */
  onMonitorMissing?: () => void;
  onFrame?: () => void;
  /** Whether the host's pointer is drawn over the picture now (only then may the local cursor hide:
   *  a host that reports its pointer hidden, e.g. Windows suppressing it after touch or pen input,
   *  would otherwise leave you with no pointer at all). */
  onPointerShown?: (on: boolean) => void;
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
  const callbacks = useRef({ onMonitors, onMonitorMissing, onFrame, onSize, onScreen, onStats, onRestart, onError, onPointerShown });
  callbacks.current = { onMonitors, onMonitorMissing, onFrame, onSize, onScreen, onStats, onRestart, onError, onPointerShown };
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
    let pointerShown = false;
    const showPointer = (shown: boolean) => {
      if (shown === pointerShown) return;
      pointerShown = shown;
      callbacks.current.onPointerShown?.(shown);
    };
    const movePointer = () => {
      if (!stopped && pointerAt && view?.screen) showPointer(pointerLayer()?.move(pointerAt, view.screen) ?? false);
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
            fps: +stats.decodedFps.toFixed(1), shown: +stats.presentedFps.toFixed(1), kbps: Math.round(stats.receivedKbps),
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
      showPointer(false);
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
        { monitor, maxFps, maxLongEdge, bitrateKbps, codecs: preferredCodecs(), onPong: clock.pong, onEnded: end,
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
          if (v) callbacks.current.onMonitors?.(v.monitors, v.monitor ?? monitor);
          callbacks.current.onScreen?.(v?.screen ?? null);
          if (v) stopPinging = startPinging(clock, (n) => v.ping(n));
          movePointer();
        }
      })
      .catch((e) => {
        if (stopped) return;
        void commands.logFromPage("error", `Cannot start live video: ${String(e)}`);
        const reason = e instanceof Error ? e.message : String(e);
        end(reason);
        // A monitor remembered from before it was unplugged: the page goes back to the primary one.
        if (monitor !== null && isMissingMonitor(reason)) callbacks.current.onMonitorMissing?.();
      });
    return () => {
      stopped = true;
      stopPinging();
      pointer?.hide();
      showPointer(false);
      view?.stop();
      decoder.close();
      callbacks.current.onScreen?.(null);
      callbacks.current.onStats?.(null);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host, monitor, maxFps, maxLongEdge, bitrateKbps, retryGeneration]);
  if (api.kind !== "tauri") {
    // The preview's computer has three monitors, so the picker shows; each one shows the same picture.
    const shown = demoMonitors.find((m) => m.index === monitor) ?? demoMonitors[0];
    return <img key={`${host}/${monitor}/${maxFps}/${maxLongEdge}/${bitrateKbps}/${retryGeneration}`} className="rd-holder" src={demoScreenUrl()} alt="" style={{ width: "100%", height: "100%", objectFit: "cover" }} onLoad={() => { callbacks.current.onSize?.(shown.width, shown.height); callbacks.current.onFrame?.(); callbacks.current.onMonitors?.(demoMonitors, shown.index); callbacks.current.onScreen?.({ x: shown.x ?? 0, y: shown.y ?? 0, width: shown.width, height: shown.height }); }} />;
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

/** The floating bar over the picture: a dot, a short status, the actions. Small and dark in both themes;
 *  the route and the latency live in the HUD. */
function DriverBar({
  bar,
  agent,
  busy,
  onAction,
  immersive,
  onLeave,
  ownerPaused = false,
  optionsOpen,
  onOptions,
}: {
  bar: Bar;
  agent: string;
  busy: boolean;
  onAction: (a: "take_over" | "pause" | "hand_back" | "resume") => void;
  immersive: boolean;
  onLeave: () => void;
  /** The owner paused it at that computer: only they can resume. */
  ownerPaused?: boolean;
  optionsOpen: boolean;
  onOptions: () => void;
}) {
  const status: Record<Bar, string> = {
    agent: t("drv_agent", { agent }),
    waiting: t("drv_waiting"),
    you: t("drv_you"),
    paused: t("drv_paused"),
    idle: t("drv_idle"),
    offline: t("drv_offline"),
    connecting: t("drv_connecting"),
  };
  const take = (
    <button className="btn sm primary" disabled={busy} onClick={() => onAction("take_over")}>
      {t("take_over")}
    </button>
  );
  const pause = (
    <button className="btn sm" disabled={busy} onClick={() => onAction("pause")}>
      <Glyph name="paused" size={13} />
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
      <button className="btn sm primary" disabled={busy} onClick={() => onAction("hand_back")} title="Ctrl+Alt+H">
        {t("hand_back")}
      </button>
    ),
    paused: (
      <button className="btn sm primary" disabled={busy || ownerPaused} title={ownerPaused ? t("owner_paused_hint") : undefined} onClick={() => onAction("resume")}>
        <Icon name="play" size={13} />
        {t("resume")}
      </button>
    ),
    offline: null,
    connecting: pause,
  };
  return (
    <div className="nt-bar bottom">
      <i className={cx("nt-dot", bar)} />
      <span className="nt-status">{status[bar]}</span>
      {actions[bar]}
      {bar === "you" && (
        <button type="button" className="nt-iconbtn" onClick={onOptions} aria-label={t("takeover_options")} aria-expanded={optionsOpen} aria-controls="takeover-options">
          <Icon name="settings" size={14} />
        </button>
      )}
      {immersive && (
        <button className="nt-iconbtn" onClick={onLeave} aria-label={t("fs_exit")} title={`${t("fs_exit")} · ⌃⌘F`}>
          <Icon name="x" size={14} />
        </button>
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
        <span title={t("hud_fps_detail", { decoded: stats.decodedFps.toFixed(0) })}>
          <b>{stats.presentedFps.toFixed(0)}</b>
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
  active = true,
  approvalRef,
  approvalVisible = true,
  onImmersiveChange,
}: {
  c: Computer;
  device: string;
  waiting: number;
  onBack: () => void;
  onNav: (n: Nav | "home") => void;
  /** A hidden window must not keep watching, even when the workspace is its current route. */
  active?: boolean;
  /** The host approval beside the screen, owned by the shared live/workspace frame. */
  approvalRef?: React.RefObject<HTMLDivElement | null>;
  approvalVisible?: boolean;
  onImmersiveChange?: (immersive: boolean) => void;
}) {
  void waiting;
  void onNav;
  void device;
  const now = useNow(1000);
  const zh = currentLang() === "zh";
  const host = c.host.name;
  const agent = agentLabel(c.feed.agent);
  const [stageRef, stage] = useSize<HTMLDivElement>();
  const [native, setNative] = useState<{ w: number; h: number } | null>(null);
  const [hasFrame, setHasFrame] = useState(false);
  const [screen, setScreen] = useState<ScreenRect | null>(null);
  const [quality, setQuality] = useState(loadStreamQuality);
  // The host's monitor to show (null: the primary one), remembered per computer; the host's list and
  // the index it streams come with each start and stay while the next stream starts.
  const [monitor, setMonitor] = useState(() => loadMonitor(host));
  const [monitors, setMonitors] = useState<RemoteMonitor[]>([]);
  const [streamed, setStreamed] = useState<number | null>(null);
  useEffect(() => {
    setMonitor(loadMonitor(host));
    setMonitors([]);
    setStreamed(null);
  }, [host]);
  // Concrete limits ("auto" follows this display), fixed while the quality stays the same.
  const limits = useMemo(() => resolveQuality(quality), [quality]);
  const [stats, setStats] = useState<VideoStats | null>(null);
  const [handNote, setHandNote] = useState("");
  const [mayWatch, setMayWatch] = useState(false);
  const [driverOptions, setDriverOptions] = useState(false);
  const [reviewApproval, setReviewApproval] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  // A passing note from live input (paused, a refused chord) and file transfers.
  const [notice, setNotice] = useState<string | null>(null);
  const [pointerShown, setPointerShown] = useState(false);
  // Files on the host's clipboard (paths there) and on this computer's.
  const [remoteFiles, setRemoteFiles] = useState<string[]>([]);
  const [localFiles, setLocalFiles] = useState<string[]>([]);
  const [transfer, setTransfer] = useState<{ name: string; done: number; total: number; direction: "push" | "pull" } | null>(null);
  const transferring = useRef(false);
  const [acting, setActing] = useState(false);
  const [immersive, setImmersive] = useState(false);
  // In full screen the bars show on movement and leave after 3 s of quiet.
  const [chrome, setChrome] = useState(true);
  const chromeTimer = useRef<number | null>(null);
  const ownCardRef = useRef<HTMLDivElement>(null);
  const cardRef = approvalRef ?? ownCardRef;
  useEffect(() => { onImmersiveChange?.(immersive); }, [immersive, onImmersiveChange]);

  const resetVideo = () => {
    setHasFrame(false);
    setScreen(null);
    setStats(null);
    setNative(null);
  };

  useEffect(() => {
    if (c.online === false || !active) resetVideo();
  }, [c.online, active]);

  const bar = barOf(c, hasFrame);
  const pending = c.feed.pending[0];
  const driving = bar === "you";

  // Full screen: ours (⌃⌘F, the button) and the window's (the green button) end in the same state.
  const ownChangeAt = useRef(0);
  const enter = (on: boolean) => {
    ownChangeAt.current = performance.now();
    setImmersive(on);
    setChrome(true);
    void setFullscreen(on).catch((e) => setActionError(e instanceof Error ? e.message : String(e)));
  };
  useEffect(() => {
    void isFullscreen().then((on) => on && setImmersive(true));
    return watchFullscreen((on) => {
      // Our own change resizes the window too; the window's answer can lag behind the transition.
      if (performance.now() - ownChangeAt.current < 1500) return;
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
  // The bars leave after 3 s of quiet, but not while the pointer rests on one or its quality menu is
  // open (the open menu sends the page no movement).
  const hideSoon = () => {
    if (chromeTimer.current) window.clearTimeout(chromeTimer.current);
    chromeTimer.current = window.setTimeout(() => {
      if (stageRef.current?.querySelector(".nt-anchor:hover, .nt-anchor:focus-within, .nt-drive-options:hover, .nt-drive-options:focus-within")) hideSoon();
      else setChrome(false);
    }, 3000);
  };
  const touch = (e?: React.MouseEvent) => {
    // While you drive, only the edges bring the bars back (they cover remote pixels).
    if (driving && e) {
      const r = e.currentTarget.getBoundingClientRect();
      const y = e.clientY - r.top;
      if (y > 56 && y < r.height - 72) return;
    }
    setChrome(true);
    hideSoon();
  };
  useEffect(() => {
    touch();
    return () => {
      if (chromeTimer.current) window.clearTimeout(chromeTimer.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [immersive, bar]);
  const showBars = chrome || (driving && driverOptions);
  useEffect(() => {
    if (!driving) setDriverOptions(false);
  }, [driving]);
  // Wait for the rail to mount after leaving full screen before locating the approval.
  useEffect(() => {
    if (!reviewApproval || immersive || !approvalVisible) return;
    cardRef.current?.scrollIntoView({ behavior: "smooth", block: "nearest" });
    cardRef.current?.focus({ preventScroll: true });
    setReviewApproval(false);
  }, [reviewApproval, immersive, approvalVisible]);

  // The picture: contained in the stage (the letterbox on one axis only), keeps the remote's aspect,
  // its edges on whole device pixels so nothing is resampled at the border.
  const aspect = native ? native.w / native.h : 16 / 10;
  const dpr = window.devicePixelRatio || 1;
  let sw = stage.w;
  let sh = sw / aspect;
  if (sh > stage.h) {
    sh = stage.h;
    sw = sh * aspect;
  }
  sw = Math.round(sw * dpr) / dpr;
  sh = Math.round(sh * dpr) / dpr;
  const left = Math.round(((stage.w - sw) / 2) * dpr) / dpr;
  const top = Math.round(((stage.h - sh) / 2) * dpr) / dpr;

  const baseName = (path: string) => path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
  // Transfers waiting their turn: files dropped (or picked) while one runs are queued, not lost.
  const transferQueue = useRef<Array<{ path: string; direction: "push" | "pull" }>>([]);
  /** Sends files to the host's desktop, or fetches them to Downloads, one after another. */
  const moveFiles = async (paths: string[], direction: "push" | "pull") => {
    transferQueue.current.push(...paths.map((path) => ({ path, direction })));
    if (transferring.current || transferQueue.current.length === 0) return;
    transferring.current = true;
    setActionError(null);
    let last = "";
    while (transferQueue.current.length) {
      const job = transferQueue.current.shift()!;
      const name = baseName(job.path);
      try {
        setTransfer({ name, done: 0, total: 0, direction: job.direction });
        const progress = (done: number, total: number) => setTransfer({ name, done, total, direction: job.direction });
        const result =
          job.direction === "push" ? await api.pushFile(host, job.path, null, progress) : await api.pullFile(host, job.path, progress);
        last =
          job.direction === "push"
            ? t("transfer_sent", { name, path: result.remote ?? "" })
            : t("transfer_received", { name, path: result.local ?? "" });
        if (job.direction === "pull") setRemoteFiles((files) => files.filter((f) => f !== job.path));
        else setLocalFiles((files) => files.filter((f) => f !== job.path));
      } catch (e) {
        setActionError(t("transfer_failed", { error: `${name}: ${e instanceof Error ? e.message : String(e)}` }));
      }
    }
    if (last) setNotice(last);
    transferring.current = false;
    setTransfer(null);
  };
  // Files dropped on the window go to the host's desktop.
  useEffect(() => {
    if (c.online === false) return;
    return watchFileDrop((paths) => void moveFiles(paths, "push"));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host, c.online]);
  // Notes fade by themselves; errors stay until the next action.
  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), 6000);
    return () => window.clearTimeout(timer);
  }, [notice]);
  useEffect(() => {
    if (!driving) {
      setLocalFiles([]);
      setRemoteFiles([]);
    }
  }, [driving]);

  const act = async (a: "take_over" | "pause" | "hand_back" | "resume") => {
    if (acting) return;
    setActing(true);
    setActionError(null);
    try {
      if (a === "take_over") await api.setTurn(host, "take_over", { agentMayWatch: mayWatch });
      else if (a === "hand_back") {
        await api.setTurn(host, "hand_back", { note: handNote.trim() || undefined });
        setHandNote("");
        setDriverOptions(false);
      } else await api.setTurn(host, a);
    } catch (e) {
      setActionError(e instanceof Error ? e.message : String(e));
    } finally {
      setActing(false);
    }
  };

  const qualityPicker = (
    <span className="nt-select-wrap">
      <select
        id="stream-quality"
        className="nt-select"
        aria-label={t("stream_quality")}
        value={quality.id}
        title={`${t("stream_requested_limits", { edge: limits.maxLongEdge, fps: limits.maxFps, mbps: limits.bitrateKbps / 1000 })}\n${t("stream_requested_note")}`}
        onChange={(e) => {
          // Let go of the focus, or the bar it sits on would stay up.
          e.currentTarget.blur();
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

  // Only a host with more than one monitor gets the picker; options read left to right as on its desk.
  const shownMonitor = monitor ?? streamed ?? primaryOf(monitors)?.index ?? null;
  const names = monitorNames(monitors, t);
  const monitorPicker = monitors.length > 1 && (
    <span className="nt-select-wrap">
      <select
        className="nt-select"
        aria-label={t("stream_monitor")}
        value={shownMonitor ?? ""}
        title={t("stream_monitor_hint", { n: monitors.length })}
        onChange={(e) => {
          // Let go of the focus, or the bar it sits on would stay up.
          e.currentTarget.blur();
          const next = Number(e.target.value);
          if (!Number.isInteger(next) || next === shownMonitor) return;
          resetVideo();
          saveMonitor(host, next);
          setMonitor(next);
        }}
      >
        {pickerOrder(monitors).map((m) => <option key={m.index} value={m.index}>{monitorOption(m, names)}</option>)}
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
            {monitorPicker}
            {qualityPicker}
            <button className="nt-iconbtn" onClick={() => enter(true)} aria-label={t("fs_enter")} title={`${t("fs_enter")} · ⌃⌘F`}>
              <Icon name="fullscreen" size={16} />
            </button>
          </>
        }
      />
      <div className="nt-session-body">
        {/* The local cursor always shows on the stage; over the picture the input layer hides it only
            while the host's own pointer is drawn. Hiding it on the whole stage lost it on the black
            edges, on the tab and over "video ended" while you drove in full screen. */}
        <section
          className="nt-stage"
          ref={stageRef}
          onMouseMove={touch}
          onMouseDown={() => immersive && !driving && touch()}
        >
          <div className={cx("nt-picture", bar === "offline" && "desat")} style={{ left, top, width: sw, height: sh }}>
            {c.online !== false && active && (
              <Stream
                host={host}
                monitor={monitor}
                onMonitors={(list, index) => {
                  setMonitors(list);
                  setStreamed(index);
                }}
                onMonitorMissing={() => {
                  saveMonitor(host, null);
                  setMonitor(null);
                  setNotice(t("monitor_gone"));
                }}
                maxFps={limits.maxFps}
                maxLongEdge={limits.maxLongEdge}
                bitrateKbps={limits.bitrateKbps}
                onFrame={() => setHasFrame(true)}
                onSize={(w, h) => setNative({ w, h })}
                onScreen={setScreen}
                onStats={setStats}
                onRestart={resetVideo}
                onError={setActionError}
                onPointerShown={setPointerShown}
              />
            )}
            {active && driving && hasFrame && screen !== null && (
              <RemoteInput
                host={host}
                screen={screen}
                mac={c.os === "mac"}
                remotePointer={pointerShown}
                onHandBack={() => void act("hand_back")}
                onError={setActionError}
                onNotice={setNotice}
                onRemoteFiles={setRemoteFiles}
                onLocalFiles={setLocalFiles}
              />
            )}
          </div>
          {actionError && <div className="session-error" role="alert">{actionError}</div>}
          {(transfer || notice || remoteFiles.length > 0 || localFiles.length > 0) && (
            <div className={cx("nt-notice", actionError && "below-error")} role="status">
              {transfer ? (
                <span>
                  {t(transfer.direction === "push" ? "transfer_sending" : "transfer_receiving", {
                    name: transfer.name,
                    pct: transfer.total ? Math.floor((transfer.done / transfer.total) * 100) : 0,
                  })}
                </span>
              ) : (
                <>
                  {notice && <span>{notice}</span>}
                  {remoteFiles.length > 0 && (
                    <button type="button" className="btn sm" onClick={() => void moveFiles(remoteFiles, "pull")}>
                      {t("clip_remote_files", { n: remoteFiles.length })}
                    </button>
                  )}
                  {localFiles.length > 0 && (
                    <button type="button" className="btn sm" onClick={() => void moveFiles(localFiles, "push")}>
                      {t("clip_local_files", { n: localFiles.length })}
                    </button>
                  )}
                </>
              )}
            </div>
          )}
          <AnimatePresence initial={false}>
            {(showBars || !immersive) && (
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
                {monitorPicker}
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
                    setReviewApproval(true);
                  }}
                >
                  {t("review")}
                </button>
                </div>
              </motion.div>
            )}
            {showBars && (
              <motion.div key="bar" {...fade} className={cx("nt-anchor bottom", driving && "raised")}>
                    <DriverBar bar={bar} agent={agent} busy={acting} onAction={act} immersive={immersive} onLeave={() => enter(false)} ownerPaused={c.feed.driver.state === "paused" && !!c.feed.driver.owner} optionsOpen={driverOptions} onOptions={() => setDriverOptions((open) => !open)} />
              </motion.div>
            )}
          </AnimatePresence>
          {driving && driverOptions && (
            <div className="nt-drive-options" id="takeover-options" role="region" aria-label={t("takeover_options")}
              onKeyDown={(e) => {
                if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); setDriverOptions(false); }
              }}>
              <div className="nt-drive-options-hd">
                <strong>{t("takeover_options")}</strong>
                <button type="button" className="nt-iconbtn" onClick={() => setDriverOptions(false)} aria-label={t("pc_close")}>
                  <Icon name="x" size={14} />
                </button>
              </div>
              <label className="nt-drive-watch">
                <span>{t("let_watch")}</span>
                <input type="checkbox" checked={mayWatch} onChange={(e) => {
                  const next = e.target.checked;
                  setMayWatch(next);
                  void api.setTurn(host, "take_over", { agentMayWatch: next }).catch((error) => setActionError(error instanceof Error ? error.message : String(error)));
                }} />
              </label>
              <label className="nt-drive-note">
                <span>{t("what_changed")}</span>
                <textarea className="ta ta-input" rows={2} value={handNote} onChange={(e) => setHandNote(e.target.value)} />
              </label>
              <p>{t("optional_sent")}</p>
              <p>{t("handback_note")}</p>
            </div>
          )}
          {/* While you drive, a small tab stays under the bottom bar: moving over the middle of the
              picture is remote input, and in full screen the screen's own edges also bring up the Dock
              and the menu bar. It only ever shows the bars, and it never moves, so a click aimed at it
              while the bars slide in cannot land on the remote screen. */}
          {driving && (
            <div className="nt-anchor handle">
              <button className="nt-handle" onMouseEnter={() => touch()} onClick={() => touch()} aria-label={t("show_controls")} title={t("show_controls")}>
                <Icon name="chevU" size={12} />
              </button>
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
      </div>
    </div>
  );
}
