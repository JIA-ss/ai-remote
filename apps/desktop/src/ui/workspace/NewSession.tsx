// The new-session page (clean-room spec NS): in the middle, a card of recent projects (a computer and a
// folder) with "Choose environment" (the online computers) and "Browse folders…" under it, and a card of
// next steps (sessions blocked on you, failed ones in this folder, choosing a folder); at the bottom the
// composer and, under it, chips: computer · folder · permission mode (Claude) and, on the right, the
// agent and its model. Sending the first message starts the session (`chat_start` with the message)
// and opens it; ⌘↵ starts it and stays here.

import { ArrowUp, ChevronDown, Code, ScrollText, ShieldCheck, TriangleAlert, X } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { folderChoices, folderName, lastCwd, recentCwds, rememberCwd, sameFolder, type Runtime, type RuntimesReport } from "../../lib/chat";
import { isCodexRuntime } from "../../lib/codex";
import { t } from "../../lib/i18n";
import type { Computer } from "../../lib/store";
import { compactEnvironment, isRunning, keepOrder, lastSeq, normalizeMeta, recentPlaces, seatSteps, type Place, type WsItem, type WsSnapshot } from "../../lib/workspace";
import { FolderPicker, type Listing } from "../chat/FolderPicker";
import { cx } from "../util";
import { ComputerMark, isMac, modKey, modLabel, PopMenu, workspaceOwnsEvent, type MenuAt, type MenuLine, type Toast } from "./parts";
import { titleOf } from "./Sidebar";
import { call, workspace } from "./useWorkspace";

/** Where sessions were started from here, newest first (NS-27: up to 8). */
const PLACES = "ai-remote.ws.places";
/** The agent last started on each computer. */
const AGENT = "ai-remote.ws.agent";
/** Next steps closed by hand: kind, subject and a stamp; a new stamp brings the step back (NS-14). */
const DISMISSED = "ai-remote.ws.dismissed";

type Started = Place & { at: number };

function readJson<V>(key: string, fallback: V): V {
  try {
    const v = localStorage.getItem(key);
    return v ? (JSON.parse(v) as V) : fallback;
  } catch {
    return fallback;
  }
}

function writeJson(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Kept for this run only.
  }
}

function rememberPlace(p: Place) {
  const list = readJson<Started[]>(PLACES, []).filter((x) => !(x.host === p.host && sameFolder(x.cwd, p.cwd)));
  writeJson(PLACES, [{ ...p, at: Date.now() }, ...list].slice(0, 8));
}

/** The installed agents of each computer, asked once per run of the page. */
const runtimesOf = new Map<string, Promise<RuntimesReport>>();
function useRuntimes(host: string | null, online: boolean) {
  const [state, setState] = useState<{ host: string; report: RuntimesReport | null; error: string | null } | null>(null);
  useEffect(() => {
    if (!host || !online) return;
    let live = true;
    let p = runtimesOf.get(host);
    if (!p) {
      p = call<RuntimesReport>(host, "chat_runtimes", {});
      runtimesOf.set(host, p);
      // A failure is asked again next time.
      p.catch(() => runtimesOf.delete(host));
    }
    p.then(
      (report) => live && setState({ host, report, error: null }),
      (e) => live && setState({ host, report: null, error: e instanceof Error ? e.message : String(e) }),
    );
    return () => {
      live = false;
    };
  }, [host, online]);
  return state && state.host === host ? state : null;
}

/** Claude's permission modes, as `chat_start` takes them (design 1.4). */
const MODES = ["default", "acceptEdits", "plan", "bypassPermissions"] as const;
type PermissionMode = (typeof MODES)[number];
/** NS-30: a scroll for plan, code for accepting edits, a warning for bypassing (the chip itself stays plain). */
const MODE_ICON: Record<PermissionMode, ReactNode> = {
  default: <ShieldCheck size={15} strokeWidth={1.5} />,
  acceptEdits: <Code size={15} strokeWidth={1.5} />,
  plan: <ScrollText size={15} strokeWidth={1.5} />,
  bypassPermissions: <TriangleAlert size={15} strokeWidth={1.5} className="ws-warn-icon" />,
};

/** The prompt being written, kept while the person looks at a session and comes back. */
let draft = "";

/** A next step: a session blocked on you (waiting or failed here), or choosing a folder. */
type Step = { kind: "waiting" | "failed"; item: WsItem } | { kind: "folder" };

export function NewSession({
  snap,
  computers,
  prefill,
  corner,
  onStarted,
  onOpen,
  onPair,
  toast,
  embedded = false,
  onPlaceChange,
}: {
  snap: WsSnapshot;
  computers: Computer[];
  /** A place to start in (a group's +), or null to use the last one. */
  prefill: Place | null;
  corner: ReactNode;
  onStarted: (host: string, session: string, ticket: string) => void;
  onOpen: (x: WsItem) => void;
  onPair: () => void;
  toast: (t: Omit<Toast, "id">) => void;
  embedded?: boolean;
  /** Reports choices made here; external prefill changes never clear the first-message draft. */
  onPlaceChange?: (place: Place) => void;
}) {
  const items = useMemo(() => workspace.items().filter((x) => !x.meta.archived), [snap]);
  const [started] = useState(() => readJson<Started[]>(PLACES, []));
  const computer = (name: string | null) => computers.find((c) => c.host.name === name) ?? null;
  const isLocal = (name: string) => !!snap.computers.find((x) => x.name === name)?.local;

  // NS-27: the place given, else the last place started from whose computer is online, else the first online computer.
  const initial = (): Place | null => {
    if (prefill) return prefill;
    const live = started.find((p) => computer(p.host)?.online === true);
    if (live) return live;
    const c = computers.find((x) => x.online === true) ?? computers[0];
    return c ? { host: c.host.name, cwd: lastCwd(c.host.name) } : null;
  };
  const [host, setHost] = useState<string | null>(() => initial()?.host ?? null);
  const [cwd, setCwd] = useState(() => initial()?.cwd ?? "");
  const page = useRef<HTMLDivElement>(null);
  const placeChanged = useRef(onPlaceChange);
  placeChanged.current = onPlaceChange;
  useEffect(() => {
    if (!prefill && host) placeChanged.current?.({ host, cwd });
  }, []);
  useEffect(() => {
    if (!prefill) return;
    setHost(prefill.host);
    setCwd(prefill.cwd);
  }, [prefill?.host, prefill?.cwd]);
  // The computers arrive after the page (first start): take the place then.
  const known = computers.length > 0;
  const anyOnline = computers.some((x) => x.online === true);
  useEffect(() => {
    if (host && computer(host)) return;
    const p = initial();
    if (!p) return;
    setHost(p.host);
    setCwd(p.cwd);
    placeChanged.current?.(p);
  }, [known, anyOnline]);
  const c = computer(host);
  const online = c?.online === true;
  const rt = useRuntimes(host, online);
  const ready = rt?.report?.runtimes.filter((r) => r.verdict === "ok") ?? [];
  const [runtimeId, setRuntimeId] = useState<string | null>(null);
  const runtime: Runtime | null = ready.find((r) => r.id === runtimeId) ?? ready.find((r) => r.id === readJson<Record<string, string>>(AGENT, {})[host ?? ""]) ?? ready[0] ?? null;
  const [model, setModel] = useState("");
  const [mode, setMode] = useState<PermissionMode>("bypassPermissions");
  const [text, setText] = useState(draft);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ kind: "env" | "mode" | "agent"; at: MenuAt } | null>(null);
  const [dismissed, setDismissed] = useState<string[]>(() => readJson<string[]>(DISMISSED, []));
  const [summary, setSummary] = useState<{ steps: Step[]; at: MenuAt } | null>(null);
  const area = useRef<HTMLTextAreaElement>(null);
  const folderBox = useRef<HTMLDivElement>(null);
  const cardsBox = useRef<HTMLDivElement>(null);
  const envCard = useRef<HTMLElement>(null);
  const stepsCard = useRef<HTMLElement>(null);
  const composer = useRef<HTMLDivElement>(null);
  const environment = useRef<HTMLDivElement>(null);
  const [compactEnv, setCompactEnv] = useState(false);
  const compactEnvRef = useRef(false);
  // NS-34: measure the full labels even while only their icons are drawn. The measurement attribute
  // changes layout synchronously and is removed before paint; popup content is not part of the row.
  useLayoutEffect(() => {
    const row = environment.current;
    const lead = row?.querySelector<HTMLElement>(".ws-env-lead");
    const trail = row?.querySelector<HTMLElement>(".ws-env-trail");
    if (!row || !lead || !trail) return;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const measure = () => {
      const style = getComputedStyle(row);
      const available = row.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
      if (available <= 0) return;
      row.dataset.measuring = "";
      const required = lead.getBoundingClientRect().width + trail.getBoundingClientRect().width + (parseFloat(style.columnGap) || 0);
      delete row.dataset.measuring;
      const next = compactEnvironment(compactEnvRef.current, available, required);
      if (next !== compactEnvRef.current) {
        compactEnvRef.current = next;
        setCompactEnv(next);
      }
    };
    // Leave the current ResizeObserver delivery before switching the label layout. Measuring during
    // that delivery would resize its own observed children and defer notifications with a loop error.
    const schedule = () => {
      timer ??= setTimeout(() => { timer = null; measure(); }, 0);
    };
    measure();
    const resize = new ResizeObserver(schedule);
    resize.observe(row);
    resize.observe(lead);
    resize.observe(trail);
    // Model names, folders, computer names and language changes can require a different width.
    const text = new MutationObserver(schedule);
    text.observe(row, { childList: true, characterData: true, subtree: true });
    return () => {
      if (timer) clearTimeout(timer);
      resize.disconnect();
      text.disconnect();
    };
  }, []);
  // NS-03: the cards stand halfway down the room from the top to 2.75rem above the composer's bottom,
  // counting the environment card and one next step (3.25rem; 2.5rem without the card). Where even that
  // does not fit, next steps that do not stop sending are left out (the card too, when none is left).
  const [spacer, setSpacer] = useState(0);
  const [tight, setTight] = useState(false);
  const tightRef = useRef(false);
  const stepsFull = useRef(0);
  const hostState = host ? snap.hosts[host] : undefined;
  const oldHost = hostState?.update === false;
  const claude = runtime?.protocol === "claude-stream-json";

  useEffect(() => {
    draft = text;
  }, [text]);
  useEffect(() => area.current?.focus(), []);
  // The composer grows with its text up to min(24rem, 40 % of the window) and then scrolls (VS-52).
  useLayoutEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [text]);

  const pickHost = (name: string) => {
    if (name === host) return;
    setHost(name);
    setCwd(lastCwd(name));
    placeChanged.current?.({ host: name, cwd: lastCwd(name) });
    setRuntimeId(null);
    setError(null);
    area.current?.focus();
  };
  /** Opens the folder chip's list; `browse` goes on to the computer's folders (the card's "Browse folders…"). */
  const openFolder = (browse = false) => {
    const box = folderBox.current;
    if (!box) return;
    if (!box.querySelector(".nt-folder-pop")) box.querySelector<HTMLButtonElement>(".nt-folder-btn")?.click();
    if (browse) requestAnimationFrame(() => box.querySelector<HTMLButtonElement>(".nt-folder-list .row.browse")?.click());
  };

  // Next steps (NS-08..NS-11): every session waiting for someone, then failed sessions in this folder;
  // choosing a folder when there is none. Three places in all (NS-09).
  const key = (s: Step) => (s.kind === "folder" ? "folder" : `${s.kind}:${s.item.key}:${lastSeq(s.item.meta)}`);
  const blocked: Step[] = [
    ...items.filter((x) => x.meta.pending && x.meta.state !== "ended").map((item) => ({ kind: "waiting" as const, item })),
    ...(host && cwd
      ? items
          .filter((x) => x.host === host && sameFolder(x.meta.cwd, cwd) && x.meta.last_status === "failed" && !isRunning(x.meta) && !x.meta.pending && !workspace.acked(x))
          .map((item) => ({ kind: "failed" as const, item }))
      : []),
  ].filter((s) => !dismissed.includes(key(s)));
  const folderStep: Step[] = host && online && !cwd.trim() ? [{ kind: "folder" }] : [];
  const seats = tight ? (folderStep.length ? [{ shown: folderStep, rest: [] as Step[] }] : []) : seatSteps<Step>([folderStep, blocked], 3);
  useLayoutEffect(() => {
    const box = cardsBox.current;
    if (!box) return;
    const measure = () => {
      const rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
      const top = box.getBoundingClientRect().top;
      const bottom = composer.current ? composer.current.getBoundingClientRect().bottom - 2.75 * rem : box.getBoundingClientRect().bottom;
      const env = envCard.current?.offsetHeight ?? 0;
      const nominal = env ? env + 2 + 3.25 * rem : 2.5 * rem;
      const room = Math.max(0, (bottom - top - nominal) / 2);
      setSpacer(room);
      // The next steps as they are when all shown (kept while some are left out, so it does not flip back).
      if (!tightRef.current && stepsCard.current) stepsFull.current = stepsCard.current.offsetHeight;
      const next = stepsFull.current > 0 && room + env + 12 + stepsFull.current + 24 > box.clientHeight;
      tightRef.current = next;
      setTight(next);
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(box);
    if (envCard.current) ro.observe(envCard.current);
    if (composer.current) ro.observe(composer.current);
    if (stepsCard.current) ro.observe(stepsCard.current);
    return () => ro.disconnect();
  }, [busy, computers.length > 0, seats.length > 0]);

  // NS-19: ⌘⌥1–9 (Alt+1–9 elsewhere) picks the environment menu's computers in order, ⌘⌥0 pairs another
  // one; physical digit keys, not while composing, held keys do nothing, AltGr is not Alt.
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if (embedded && !workspaceOwnsEvent(page.current?.closest<HTMLElement>('.ws[data-embedded="true"]') ?? null, e)) return;
      if (e.defaultPrevented || e.isComposing || !/^Digit[0-9]$/.test(e.code)) return;
      const chord = isMac ? e.metaKey && e.altKey && !e.ctrlKey && !e.shiftKey : e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey;
      if (!chord) return;
      e.preventDefault();
      if (e.repeat) return;
      const n = Number(e.code.slice(5));
      setMenu(null);
      if (n === 0) onPair();
      else {
        const x = computers[n - 1];
        if (x?.online === true) pickHost(x.host.name);
      }
    };
    window.addEventListener("keydown", on, true);
    return () => window.removeEventListener("keydown", on, true);
  });
  const dismiss = (s: Step) => {
    const next = [...dismissed.filter((k) => k !== key(s)), key(s)].slice(-100);
    setDismissed(next);
    writeJson(DISMISSED, next);
    area.current?.focus();
  };

  const shown = useRef<Array<Place & { at: number }>>([]);
  const places = keepOrder(shown.current, recentPlaces(items, started, host && cwd ? { host, cwd } : null));
  shown.current = places;
  const line: { kind: "blocking" | "note"; text: string } | null = busy
    ? { kind: "note", text: t("ws_starting") }
    : error
      ? { kind: "blocking", text: t("ws_start_failed", { error }) }
      : !host
        ? null
        : !online
          ? { kind: "blocking", text: t("ws_offline_line", { host }) }
          : rt?.error
            ? { kind: "blocking", text: /unknown tool '?chat_/i.test(rt.error) ? t("ws_too_old", { host }) : rt.error }
            : rt?.report && !rt.report.host.can_start
              ? { kind: "blocking", text: rt.report.host.reason ?? t("ws_no_agents", { host }) }
              : rt?.report && !ready.length
                ? { kind: "blocking", text: t("ws_no_agents", { host }) }
                : !rt
                  ? { kind: "note", text: t("ws_looking", { host }) }
                  : null;

  const start = async (stay: boolean) => {
    const prompt = text.trim();
    if (!prompt || busy) return;
    if (!host || !online || !runtime) return;
    if (!cwd.trim()) {
      openFolder();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const v = await call<Record<string, unknown> & { session: string; ticket: string }>(host, "chat_start", {
        runtime: runtime.id,
        cwd: cwd.trim(),
        prompt,
        ...(model.trim() ? { model: model.trim() } : {}),
        ...(claude ? { permission_mode: mode } : {}),
        intent: t("hc_intent"),
      });
      rememberCwd(host, cwd.trim());
      rememberPlace({ host, cwd: cwd.trim() });
      writeJson(AGENT, { ...readJson<Record<string, string>>(AGENT, {}), [host]: runtime.id });
      const meta = normalizeMeta({ ...v, id: v.session });
      if (meta) workspace.addSession(host, { ...meta, first_text: meta.first_text ?? prompt.slice(0, 200), state: meta.state === "idle" ? "starting" : meta.state });
      workspace.rememberFirst(host, v.session, prompt);
      setText("");
      draft = "";
      if (stay) {
        toast({ text: t("ws_started_beside", { title: prompt.split(/\r?\n/)[0].slice(0, 80) }) });
        area.current?.focus();
      } else onStarted(host, v.session, v.ticket);
    } catch (e) {
      // NS-40: the prompt stays where it was, with why.
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const envMenu: MenuLine[] = [
    { heading: t("ws_computers") },
    ...computers.map((x, i) => ({
      key: x.host.name,
      label: x.host.name,
      letter: i < 9 ? String(i + 1) : undefined,
      icon: <ComputerMark os={x.os} online={x.online} />,
      checked: x.host.name === host,
      disabled: x.online !== true,
      note: t("ws_offline"),
      run: () => pickHost(x.host.name),
    })),
    "sep",
    { key: "pair", label: t("ws_pair_more"), letter: "0", run: onPair },
  ];
  const modeMenu: MenuLine[] = [
    { heading: t("ws_perm_title") },
    ...MODES.map((m) => ({ key: m, label: t(`ws_perm_${m}`), detail: t(`ws_perm_${m}_d`), icon: MODE_ICON[m], checked: m === mode, run: () => setMode(m) })),
  ];
  const agentMenu: MenuLine[] = [
    { heading: t("ws_agent") },
    ...ready.map((r) => ({ key: r.id, label: r.name, detail: r.version ?? undefined, checked: r.id === runtime?.id, run: () => { setRuntimeId(r.id); setModel(""); } })),
  ];
  const openMenu = (kind: "env" | "mode" | "agent", el: HTMLElement, side: "below" | "above") => setMenu({ kind, at: { anchor: el.getBoundingClientRect(), side } });
  const canSend = !!text.trim() && !busy && !!runtime && online;

  return (
    <div className="ws-page ws-new" ref={page}>
      <header className="ws-tb" data-tauri-drag-region>
        {corner}
        <h1 className="ws-tb-plain" data-tauri-drag-region>
          {t("ws_new_title")}
        </h1>
        <div className="ws-drag" data-tauri-drag-region />
        <div className="ws-frame-room" />
      </header>
      <div className="ws-new-body">
        <div className="ws-new-cards" ref={cardsBox}>
          <div className="ws-new-spacer" style={{ height: spacer }} />
          {!busy && (
            <div className="ws-cards">
              {computers.length > 0 && (
                <section className="ws-card ws-env-card" ref={envCard} aria-label={t("ws_recent_places")}>
                  {places.length > 0 && (
                    <ul className="ws-env-rows">
                      {places.map((pl) => {
                        const pc = computer(pl.host);
                        const cur = pl.host === host && sameFolder(pl.cwd, cwd);
                        const name = isLocal(pl.host) ? folderName(pl.cwd) : `${folderName(pl.cwd)} · ${pl.host}`;
                        return (
                          <li key={`${pl.host}\n${pl.cwd}`} aria-current={cur ? "true" : undefined}>
                            <button
                              type="button"
                              className={cx("ws-env-row", cur && "on")}
                              disabled={cur}
                              title={`${pl.host} · ${pl.cwd}`}
                              onClick={() => {
                                setHost(pl.host);
                                setCwd(pl.cwd);
                                placeChanged.current?.({ host: pl.host, cwd: pl.cwd });
                                setRuntimeId(null);
                                setError(null);
                                area.current?.focus();
                              }}
                            >
                              <ComputerMark os={pc?.os ?? null} online={pc?.online ?? null} />
                              <span className="ws-env-name">{name}</span>
                              {cur && <span className="ws-env-check" aria-hidden>✓</span>}
                            </button>
                          </li>
                        );
                      })}
                    </ul>
                  )}
                  <div className="ws-env-foot">
                    <button type="button" className="ws-env-btn" aria-haspopup="menu" onClick={(e) => openMenu("env", e.currentTarget, "below")}>
                      <span>{t("ws_choose_env")}</span>
                      <ChevronDown size={13} strokeWidth={1.5} />
                    </button>
                    <button type="button" className="ws-env-btn" disabled={!online} onClick={() => openFolder(true)}>
                      {t("ws_browse")}
                    </button>
                  </div>
                </section>
              )}
              {computers.length === 0 && (
                <section className="ws-card ws-steps">
                  <div className="ws-step static">
                    <span className="ws-step-title">{t("ws_no_computers")}</span>
                  </div>
                  <button type="button" className="ws-step" onClick={onPair}>
                    <span className="ws-step-title">{t("ws_pair")}</span>
                  </button>
                </section>
              )}
              {seats.length > 0 && (
                <section className="ws-card ws-steps" ref={stepsCard} aria-label={t("ws_next_steps")}>
                  {seats.flatMap((g, gi) => [
                    ...g.shown.map((s) => <StepRow key={key(s)} step={s} onOpen={onOpen} onFolder={() => openFolder()} onDismiss={() => dismiss(s)} />),
                    ...(g.rest.length
                      ? [
                          <button
                            key={`rest${gi}`}
                            type="button"
                            className="ws-step"
                            aria-haspopup="menu"
                            onClick={(e) => setSummary({ steps: g.rest, at: { anchor: e.currentTarget.getBoundingClientRect(), side: "below" } })}
                          >
                            <span className="ws-step-title">
                              {g.shown.length === 0
                                ? t(g.rest.length === 1 ? "ws_blocked_in_one" : "ws_blocked_in_many", { n: g.rest.length })
                                : t(g.rest.length === 1 ? "ws_blocked_more_one" : "ws_blocked_more_many", { n: g.rest.length })}
                            </span>
                            <ChevronDown size={13} strokeWidth={1.5} />
                          </button>,
                        ]
                      : []),
                  ])}
                </section>
              )}
            </div>
          )}
        </div>
        <div className="ws-new-bottom">
          <div className={cx("ws-composer", busy && "busy")} ref={composer}>
            <textarea
              ref={area}
              rows={1}
              value={text}
              placeholder={t("ws_placeholder")}
              aria-label={t("ws_placeholder")}
              onChange={(e) => {
                setText(e.target.value);
                if (error) setError(null);
              }}
              onKeyDown={(e) => {
                if (e.key !== "Enter" || e.shiftKey || e.altKey || e.nativeEvent.isComposing || e.keyCode === 229) return;
                e.preventDefault();
                void start(modKey(e));
              }}
            />
            <button
              type="button"
              className="ws-send"
              disabled={!canSend}
              aria-label={t("ws_send")}
              title={t("ws_send_keys", { mod: modLabel })}
              onClick={(e) => void start(modKey(e))}
            >
              <ArrowUp size={16} strokeWidth={2} />
            </button>
          </div>
          <div className="ws-env" ref={environment} data-compact={compactEnv || undefined}>
            <div className="ws-env-lead">
              <button
                type="button"
                className={cx("ws-chip ws-chip-location", !online && host && "off")}
                aria-haspopup="menu"
                aria-label={host ? `${t("ws_where")}: ${host}` : t("ws_choose_computer")}
                title={host ? `${t("ws_where")}: ${host}${online ? "" : ` (${t("ws_offline")})`}` : t("ws_where")}
                onClick={(e) => openMenu("env", e.currentTarget, "above")}
              >
                <ComputerMark os={c?.os ?? null} online={c?.online ?? null} />
                <span className="ws-chip-text">{host ?? t("ws_choose_computer")}</span>
              </button>
              {host && (
                <div className="ws-chip-folder" ref={folderBox} title={cwd ? t("ws_folder_tip", { path: cwd }) : t("hc_folder_choose")}>
                  <FolderPicker
                    key={host}
                    host={host}
                    value={cwd}
                    folders={folderChoices(recentCwds(host), items.filter((x) => x.host === host).map((x) => x.meta.cwd))}
                    browse={(path) => call<Listing>(host, "files_list", { path })}
                    onChange={(path) => {
                      setCwd(path);
                      placeChanged.current?.({ host, cwd: path });
                      setError(null);
                      area.current?.focus();
                    }}
                  />
                </div>
              )}
              {runtime && claude && (
                <button
                  type="button"
                  className="ws-chip"
                  aria-haspopup="menu"
                  disabled={oldHost}
                  title={oldHost ? t("ws_perm_old", { host: host ?? "" }) : t("ws_perm_title")}
                  onClick={(e) => openMenu("mode", e.currentTarget, "above")}
                >
                  <span className="ws-chip-text">{t(`ws_perm_${oldHost ? "bypassPermissions" : mode}`)}</span>
                </button>
              )}
            </div>
            <div className="ws-env-trail">
              {runtime && (
                <>
                  <button type="button" className="ws-chip" aria-haspopup="menu" disabled={ready.length < 2} title={t("ws_agent")} onClick={(e) => openMenu("agent", e.currentTarget, "above")}>
                    <span className="ws-chip-text">{runtime.name}</span>
                  </button>
                  <ModelChip key={runtime.id} value={model} onChange={setModel} codex={isCodexRuntime(runtime)} />
                </>
              )}
            </div>
          </div>
          {line && <div className={cx("ws-env-line", line.kind)}>{line.text}</div>}
        </div>
      </div>
      {menu && (
        <PopMenu
          at={menu.at}
          label={menu.kind === "env" ? t("ws_choose_env") : menu.kind === "mode" ? t("ws_perm_title") : t("ws_agent")}
          items={menu.kind === "env" ? envMenu : menu.kind === "mode" ? modeMenu : agentMenu}
          className={menu.kind === "mode" ? "wide" : undefined}
          onClose={() => setMenu(null)}
        />
      )}
      {summary && (
        <PopMenu
          at={summary.at}
          label={t("ws_next_steps")}
          className="wide"
          items={summary.steps.map((s) =>
            s.kind === "folder"
              ? { key: "folder", label: t("ws_choose_folder"), run: () => openFolder() }
              : { key: s.item.key, label: titleOf(s.item), detail: stepDetail(s), run: () => onOpen(s.item) },
          )}
          onClose={() => setSummary(null)}
        />
      )}
    </div>
  );
}

function stepDetail(s: Step): string {
  if (s.kind === "folder") return t("ws_needed_to_start");
  if (s.kind === "failed") return t("ws_hit_error");
  const p = s.item.meta.pending;
  return p === "permission" ? t("ws_needs_permission") : p === "plan" ? t("ws_plan_review") : t("ws_asked_question");
}

/** One next step (NS-10 / NS-15): "Blocked" + the session's title + why, or "Choose a folder". */
function StepRow({ step, onOpen, onFolder, onDismiss }: { step: Step; onOpen: (x: WsItem) => void; onFolder: () => void; onDismiss: () => void }) {
  if (step.kind === "folder") {
    return (
      <button type="button" className="ws-step" onClick={onFolder}>
        <span className="ws-step-title">{t("ws_choose_folder")}</span>
        <span className="ws-step-detail">{t("ws_needed_to_start")}</span>
      </button>
    );
  }
  const title = titleOf(step.item);
  const detail = stepDetail(step);
  return (
    <div className="ws-step-wrap">
      <button type="button" className="ws-step" title={`${title} · ${detail}`} onClick={() => onOpen(step.item)}>
        <span className="ws-step-title">
          <span className="ws-step-blocked">{t("ws_blocked")}</span> {title}
        </span>
        <span className="ws-step-detail">{detail}</span>
      </button>
      <button type="button" className="ws-step-x" aria-label={t("ws_dismiss")} title={t("ws_dismiss")} onClick={onDismiss}>
        <X size={13} strokeWidth={1.5} />
      </button>
    </div>
  );
}

/** The model chip: "Default model" or the one typed, set in a small field under it. */
function ModelChip({ value, onChange, codex }: { value: string; onChange: (v: string) => void; codex: boolean }) {
  const [open, setOpen] = useState(false);
  const [v, setV] = useState(value);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const down = (e: Event) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", down, true);
    return () => document.removeEventListener("pointerdown", down, true);
  }, [open]);
  return (
    <div className="ws-model" ref={box}>
      <button type="button" className={cx("ws-chip", value && "set")} aria-expanded={open} title={t("ws_model")} onClick={() => { setV(value); setOpen((o) => !o); }}>
        <span className="ws-chip-text">{value || t("ws_model_default")}</span>
      </button>
      {open && (
        <div className="ws-model-pop" role="dialog" aria-label={t("ws_model")}>
          <input
            autoFocus
            className="mono"
            value={v}
            placeholder={codex ? "gpt-…" : t("ws_model_ph")}
            aria-label={t("ws_model")}
            spellCheck={false}
            onChange={(e) => setV(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                onChange(v.trim());
                setOpen(false);
              } else if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                setOpen(false);
              }
            }}
          />
        </div>
      )}
    </div>
  );
}
