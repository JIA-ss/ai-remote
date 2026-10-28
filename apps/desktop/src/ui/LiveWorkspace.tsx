// The existing workspace beside the existing live screen. They are siblings: selecting another
// conversation keeps the video mounted; selecting another computer replaces only its screen.
import { useRef, useState } from "react";
import { lastCwd } from "../lib/chat";
import { t } from "../lib/i18n";
import { workspaceOfScreen, type AgentLocation, type Route } from "../lib/nav";
import type { Computer } from "../lib/store";
import { DecisionCard } from "./Decide";
import { SessionPage } from "./Session";
import { agentLabel, cx } from "./util";
import type { Nav } from "./Shell";
import { Workspace } from "./workspace/Workspace";

const SPLIT = "ai-remote.live.split";
const clamp = (value: number) => Math.max(28, Math.min(72, value));
function readSplit() {
  try {
    const value = Number(localStorage.getItem(SPLIT));
    return Number.isFinite(value) && value >= 28 && value <= 72 ? value : 50;
  } catch { return 50; }
}

export function LiveWorkspace({ c, computers, device, waiting, active = true, route, onRoute, back, forward, onNav, onPair, onSettings, onMode }: {
  c: Computer;
  computers: Computer[];
  device: string;
  waiting: number;
  active?: boolean;
  route: Extract<Route, { page: "session" }>;
  onRoute: (route: AgentLocation, replace?: boolean) => void;
  back: (() => void) | null;
  forward: (() => void) | null;
  onNav: (n: Nav | "home") => void;
  onPair: () => void;
  onSettings: () => void;
  onMode: () => void;
}) {
  const root = useRef<HTMLDivElement>(null);
  const approval = useRef<HTMLDivElement>(null);
  const [immersive, setImmersive] = useState(false);
  const [split, setSplit] = useState(readSplit);
  const [dragging, setDragging] = useState(false);
  // In a short window the request above the workspace is a compact card (who asks, the command, the
  // two answers, the time left; glass/stage.css); "Details" shows the reason and the rest.
  const [full, setFull] = useState(false);
  const selected = workspaceOfScreen(route);
  const location = selected.session || selected.place ? selected : { place: { host: c.host.name, cwd: lastCwd(c.host.name) } };
  const pending = c.feed.pending[0];
  // Requests waiting on the other computers: this view shows only its own computer's card, so they
  // get one line above it that opens Approvals (and a dot on the workspace's Computers switch).
  const elsewhere = computers.filter((x) => x.host.name !== c.host.name && x.feed.pending.length > 0);
  const otherCount = elsewhere.reduce((n, x) => n + x.feed.pending.length, 0);
  const attention = otherCount === 0 ? undefined
    : otherCount === 1 ? t("lv_attn_one", { host: elsewhere[0].host.name }) : t("lv_attn_many", { n: otherCount });
  const step = pending ? c.feed.steps.find((s) => s.approvalId === pending.id) : undefined;
  const save = (value: number) => {
    const next = clamp(value);
    setSplit(next);
    try { localStorage.setItem(SPLIT, String(next)); } catch { /* Keep this window usable without storage. */ }
  };
  const resize = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const bounds = root.current?.getBoundingClientRect();
    if (!bounds?.width) return;
    event.preventDefault();
    const grip = event.currentTarget;
    grip.setPointerCapture(event.pointerId);
    setDragging(true);
    let value = split;
    const move = (e: PointerEvent) => {
      value = clamp((e.clientX - bounds.left) / bounds.width * 100);
      setSplit(value);
    };
    const stop = () => {
      grip.removeEventListener("pointermove", move);
      grip.removeEventListener("pointerup", stop);
      grip.removeEventListener("pointercancel", stop);
      setDragging(false);
      save(value);
    };
    grip.addEventListener("pointermove", move);
    grip.addEventListener("pointerup", stop);
    grip.addEventListener("pointercancel", stop);
  };
  return (
    <div ref={root} className={cx("nt-live-workspace", immersive && "immersive", dragging && "resizing")}
      style={{ ["--live-screen-share" as string]: `${split}fr`, ["--live-workspace-share" as string]: `${100 - split}fr` }}>
      <SessionPage key={c.host.name} c={c} device={device} waiting={waiting} active={active} onBack={back ?? onMode} onNav={onNav}
        approvalRef={approval} approvalVisible={!immersive} onImmersiveChange={setImmersive} />
      <div className="nt-live-divider" role="separator" aria-orientation="vertical" tabIndex={0}
        aria-label={t("live_resize_workspace")} aria-valuemin={28} aria-valuemax={72} aria-valuenow={Math.round(split)}
        onPointerDown={resize} onDoubleClick={() => save(50)} onKeyDown={(e) => {
          if (e.key === "ArrowLeft" || e.key === "ArrowRight" || e.key === "Home") {
            e.preventDefault();
            e.stopPropagation();
            save(e.key === "Home" ? 50 : split + (e.key === "ArrowRight" ? 2 : -2));
          }
        }} />
      <aside className="nt-rail nt-workspace-pane" inert={immersive}>
        {attention && (
          <button type="button" className="gl-live-attn" onClick={() => onNav("approvals")}>
            <i className="gl-live-attn-dot" aria-hidden="true" />
            <span className="gl-live-attn-tx">{attention}</span>
            <span className="gl-live-attn-go">{t("devices_attention_go")}</span>
          </button>
        )}
        {pending && <div key={`${c.host.name}/${pending.id}`} className={cx("nt-chat-approval", full && "gl-full")} ref={approval} tabIndex={-1}>
          <DecisionCard p={pending} host={c.host.name} agent={agentLabel(c.feed.agent)} askedMs={step?.startedMs} escapeScope={approval}
            aside={<button type="button" className="gl-appr-more" aria-expanded={full} onClick={() => setFull((v) => !v)}>
              {t(full ? "lv_appr_less" : "lv_appr_more")}
            </button>} />
        </div>}
        <Workspace embedded computers={computers} device={device} route={location} onRoute={(r) => onRoute(r)}
          onPlaceChange={(place) => onRoute({ place }, true)}
          initialTarget={route.chat ? { host: c.host.name, ...route.chat } : null}
          back={back} forward={forward} onMode={onMode} onPair={onPair} onSettings={onSettings} attention={attention}
          onWatch={() => root.current?.querySelector<HTMLElement>(".nt-stage")?.focus()} />
      </aside>
    </div>
  );
}
