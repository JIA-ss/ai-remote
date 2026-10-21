// The chat panel beside the live view: a coding agent running on the host (Claude Code, Codex, ...),
// started from here or from the computer page, and the conversation with it. Events come from the
// host's chat channel and are folded by lib/chat.ts; text from the agent is only ever rendered as
// text nodes (never HTML), and links are not followed from here. The conversation's look follows
// Orca's native chat (see ui/chat/).

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, type ChatConnection } from "../lib/api";
import {
  editQueued,
  emptyChat,
  inputCommand,
  markAnswered,
  phase,
  reduceChat,
  rememberCwd,
  rememberSent,
  recentCwds,
  type Chat,
  type ChatEvent,
  type ChatHello,
  type InputMode,
  type Runtime,
  type RuntimesReport,
  type SessionInfo,
} from "../lib/chat";
import { t } from "../lib/i18n";
import { Composer } from "./chat/Composer";
import { secs } from "./chat/Rows";
import { Transcript } from "./chat/Transcript";
import { Icon } from "./Icon";
import { cx } from "./util";

/** The session to show, and a ticket when it was just started (a ticket is valid once). */
export interface ChatTarget {
  session: string;
  ticket?: string;
}

const LAST = "ai-remote.chat.last";

function lastSession(host: string): string | null {
  try {
    return (JSON.parse(localStorage.getItem(LAST) ?? "{}") as Record<string, string>)[host] ?? null;
  } catch {
    return null;
  }
}

function rememberSession(host: string, session: string | null) {
  try {
    const all = JSON.parse(localStorage.getItem(LAST) ?? "{}") as Record<string, string>;
    if (session) all[host] = session;
    else delete all[host];
    localStorage.setItem(LAST, JSON.stringify(all));
  } catch {
    // Nothing to remember then.
  }
}

/** The JSON of a tool result, or its error text. */
async function call<T>(host: string, tool: string, args: Record<string, unknown> = {}): Promise<T> {
  const r = await api.callTool(host, tool, args, true);
  const text = r.content.find((c) => c.type === "text")?.text ?? "";
  if (r.isError) throw new Error(text || `${tool} failed`);
  return (r.structuredContent ?? JSON.parse(text)) as T;
}

/** The installed agents of `host`, fetched once per mount (`refresh` looks again). */
export function useRuntimes(host: string) {
  const [state, setState] = useState<{ report: RuntimesReport | null; error: string | null }>({ report: null, error: null });
  const [round, setRound] = useState(0);
  useEffect(() => {
    let live = true;
    call<RuntimesReport>(host, "chat_runtimes", { refresh: round > 0 }).then(
      (report) => live && setState({ report, error: null }),
      (e) => live && setState({ report: null, error: String(e instanceof Error ? e.message : e) }),
    );
    return () => {
      live = false;
    };
  }, [host, round]);
  return { ...state, refresh: () => setRound((r) => r + 1) };
}

export function useSessions(host: string, tick = 0) {
  const [sessions, setSessions] = useState<SessionInfo[] | null>(null);
  useEffect(() => {
    let live = true;
    call<{ sessions: SessionInfo[] }>(host, "chat_sessions").then(
      (v) => live && setSessions(v.sessions),
      () => live && setSessions([]),
    );
    return () => {
      live = false;
    };
  }, [host, tick]);
  return sessions;
}

/** "Signed in (subscription)" and similar, for a runtime row. */
export function loginLabel(r: Runtime): string | null {
  const l = r.login;
  if (!l) return null;
  if (l.state === "logged_in") return l.method ? t("hc_signed_in_by", { m: l.method }) : t("hc_signed_in");
  if (l.state === "logged_out") return t("hc_signed_out");
  return null;
}

/** Starting an agent: which one, in which folder, with an optional first message. */
export function StartForm({
  host,
  report,
  compact,
  onStarted,
}: {
  host: string;
  report: RuntimesReport;
  compact?: boolean;
  onStarted: (target: ChatTarget) => void;
}) {
  const ready = report.runtimes.filter((r) => r.verdict === "ok");
  const [runtime, setRuntime] = useState(ready[0]?.id ?? "");
  const recents = recentCwds(host);
  const [cwd, setCwd] = useState(recents[0] ?? "");
  const [prompt, setPrompt] = useState("");
  const [model, setModel] = useState("");
  const [hardened, setHardened] = useState(false);
  const [more, setMore] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!report.host.can_start) return <div className="nt-chat-note warn">{report.host.reason}</div>;
  if (report.host.config_error) return <div className="nt-chat-note err">{t("hc_config_error", { host, error: report.host.config_error })}</div>;
  if (report.host.enabled === false) return <div className="nt-chat-note warn">{t("hc_disabled", { host })}</div>;
  if (!ready.length) return <div className="nt-chat-note">{t("hc_none", { host })}</div>;
  const start = async () => {
    if (!cwd.trim()) {
      setError(t("hc_cwd_needed"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const v = await call<{ session: string; ticket: string }>(host, "chat_start", {
        runtime,
        cwd: cwd.trim(),
        ...(prompt.trim() ? { prompt: prompt.trim() } : {}),
        ...(model.trim() ? { model: model.trim() } : {}),
        hardened,
        intent: t("hc_intent"),
      });
      rememberCwd(host, cwd.trim());
      onStarted({ session: v.session, ticket: v.ticket });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className={cx("nt-chat-start", compact && "compact")}>
      <label className="lbl">{t("hc_which")}</label>
      <div className="nt-chat-pick">
        {ready.map((r) => (
          <button key={r.id} type="button" className={cx("opt", r.id === runtime && "sel")} onClick={() => setRuntime(r.id)} aria-pressed={r.id === runtime}>
            <span className="nm">{r.name}</span>
            <span className="vs mono">{r.version ?? ""}</span>
          </button>
        ))}
      </div>
      <label className="lbl" htmlFor="ag-cwd">
        {t("hc_cwd")}
      </label>
      <input id="ag-cwd" className="ta-input mono" list="ag-cwd-recent" value={cwd} placeholder={t("hc_cwd_ph")} onChange={(e) => setCwd(e.target.value)} spellCheck={false} />
      <datalist id="ag-cwd-recent">
        {recents.map((r) => (
          <option key={r} value={r} />
        ))}
      </datalist>
      <label className="lbl" htmlFor="ag-prompt">
        {t("hc_first")}
      </label>
      <textarea id="ag-prompt" className="ta ta-input" rows={compact ? 2 : 3} value={prompt} placeholder={t("hc_first_ph")} onChange={(e) => setPrompt(e.target.value)} />
      <button type="button" className="nt-chat-more" onClick={() => setMore((v) => !v)} aria-expanded={more}>
        <Icon name={more ? "chevD" : "chevR"} size={12} />
        {t("hc_more")}
      </button>
      {more && (
        <div className="nt-chat-adv">
          <label className="lbl" htmlFor="ag-model">
            {t("hc_model")}
          </label>
          <input id="ag-model" className="ta-input mono" value={model} placeholder={t("hc_model_ph")} onChange={(e) => setModel(e.target.value)} spellCheck={false} />
          <label className="chk">
            <input type="checkbox" checked={hardened} onChange={(e) => setHardened(e.target.checked)} />
            <span>{t("hc_hardened")}</span>
          </label>
        </div>
      )}
      <div className="nt-chat-warn">
        <Icon name="warn" size={14} />
        <span>{t("hc_bypass_warn", { host })}</span>
      </div>
      {error && <div className="nt-chat-note err">{error}</div>}
      <button type="button" className="btn primary" disabled={busy || !runtime} onClick={() => void start()}>
        <Icon name="play" size={14} />
        {busy ? t("hc_starting") : t("hc_start")}
      </button>
    </div>
  );
}

function useNowTick(active: boolean) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!active) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [active]);
  return now;
}

/** The conversation with one session: events from the host, folded, with reconnection. */
function useConversation(host: string, target: ChatTarget | null) {
  const [chat, setChat] = useState<Chat>(emptyChat);
  const [hello, setHello] = useState<ChatHello | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [connected, setConnected] = useState(false);
  const conn = useRef<ChatConnection | null>(null);
  const chatRef = useRef(chat);
  chatRef.current = chat;
  useEffect(() => {
    if (!target) return;
    let live = true;
    let ticket = target.ticket;
    let retry: ReturnType<typeof setTimeout> | null = null;
    // Events are folded at most once per frame: a fast agent must not re-render per token while
    // the live view keeps the compositor busy.
    let pending: ChatEvent[] = [];
    let frame = 0;
    const flush = () => {
      frame = 0;
      const batch = pending;
      pending = [];
      setChat((c) => batch.reduce(reduceChat, c));
    };
    setChat(emptyChat());
    setHello(null);
    const open = async () => {
      try {
        if (!ticket) ticket = (await call<{ ticket: string }>(host, "chat_attach", { session: target.session })).ticket;
        const c = await api.openChat(host, ticket, chatRef.current.seq, (e) => {
          pending.push(e);
          if (!frame) frame = requestAnimationFrame(flush);
        }, () => {
          if (!live) return;
          setConnected(false);
          conn.current = null;
          retry = setTimeout(() => void open(), 3000);
        });
        ticket = undefined;
        if (!live) {
          c.close();
          return;
        }
        conn.current = c;
        setHello(c.hello);
        setConnected(true);
        setError(null);
      } catch (e) {
        if (!live) return;
        if (ticket) {
          // A ticket is valid once: one handed over with the target may already be used (the view
          // was mounted again). Ask for a fresh one.
          ticket = undefined;
          void open();
          return;
        }
        const msg = e instanceof Error ? e.message : String(e);
        setError(msg);
        if (!/no chat session|requires level|no longer has access/.test(msg)) retry = setTimeout(() => void open(), 5000);
      }
    };
    void open();
    return () => {
      live = false;
      if (retry) clearTimeout(retry);
      if (frame) cancelAnimationFrame(frame);
      conn.current?.close();
      conn.current = null;
    };
  }, [host, target?.session]);
  const send = (command: Record<string, unknown>) => {
    if (!conn.current) {
      setError(t("hc_not_connected"));
      return;
    }
    void conn.current.send(command).catch((e) => setError(String(e)));
  };
  return { chat, setChat, hello, error, connected, send };
}

/** The chat tab of the session page. */
export function AgentChat({ host, target, onTarget }: { host: string; target: ChatTarget | null; onTarget: (t: ChatTarget | null) => void }) {
  const current = target ?? (lastSession(host) ? { session: lastSession(host) as string } : null);
  if (!current) return <Picker host={host} onPick={onTarget} />;
  return (
    <Conversation
      key={current.session}
      host={host}
      target={current}
      onLeave={() => {
        rememberSession(host, null);
        onTarget(null);
      }}
    />
  );
}

/** No session open: the sessions on the host, and starting a new one. */
function Picker({ host, onPick }: { host: string; onPick: (t: ChatTarget) => void }) {
  const { report, error } = useRuntimes(host);
  const sessions = useSessions(host);
  const open = (sessions ?? []).filter((s) => s.state !== "ended");
  const pick = (t: ChatTarget) => {
    rememberSession(host, t.session);
    onPick(t);
  };
  return (
    <div className="nt-chat-picker rail-scroll">
      {open.length > 0 && (
        <>
          <div className="grp-h">{t("hc_sessions")}</div>
          <div className="nt-sessions">
            {open.map((s) => (
              <button key={s.id} type="button" className="sess" onClick={() => pick({ session: s.id })}>
                <i className={cx("dot", s.state)} />
                <span className="nm">{s.name}</span>
                <span className="cwd mono">{s.cwd}</span>
                <span className="n">{t("hc_turns", { n: s.turns })}</span>
              </button>
            ))}
          </div>
        </>
      )}
      <div className="grp-h">{t("hc_new")}</div>
      {error ? <div className="nt-chat-note err">{error}</div> : report ? <StartForm host={host} report={report} compact onStarted={pick} /> : <div className="nt-chat-note">{t("hc_looking", { host })}</div>}
    </div>
  );
}


const newId = () => `u${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;

/** Sent messages per session, for ↑ / ↓ (kept for this run of the app). */
const sentBySession = new Map<string, string[]>();

function Conversation({ host, target, onLeave }: { host: string; target: ChatTarget; onLeave: () => void }) {
  const { chat, setChat, hello, error, connected, send } = useConversation(host, target);
  const [menu, setMenu] = useState(false);
  const [history, setHistory] = useState<string[]>(() => sentBySession.get(target.session) ?? []);
  const [follow, setFollow] = useState(0);
  const running = chat.state === "running" || chat.state === "starting" || chat.state === "interrupting";
  const now = useNowTick(running);
  const readonly = hello?.role === "ro";
  const ended = chat.state === "ended";
  const agent = hello?.runtime.name ?? t("hc_agent");
  const canSteer = !!hello?.caps?.steer;
  const ph = phase(chat);
  const sendRef = useRef(send);
  sendRef.current = send;

  const remember = (text: string) => {
    setHistory((h) => {
      const next = rememberSent(h, text);
      sentBySession.set(target.session, next);
      return next;
    });
  };
  const sendText = (text: string, mode: InputMode | undefined) => {
    send(inputCommand(newId(), text, mode));
    remember(text);
    setFollow((n) => n + 1);
  };
  const onAnswer = useCallback((qid: string, value: string) => {
    sendRef.current({ answer: { qid, text: value } });
    setChat((c) => markAnswered(c, qid, value));
    setFollow((n) => n + 1);
  }, [setChat]);
  const onContinue = useCallback(() => {
    sendRef.current(inputCommand(newId(), t("hc_continue_prompt")));
    setFollow((n) => n + 1);
  }, []);

  const working = useMemo(() => {
    switch (ph.what) {
      case "starting":
        return t("hc_st_starting");
      case "stopping":
        return t("hc_st_stopping");
      case "tool":
        return ph.title ?? t("hc_working");
      case "writing":
        return t("hc_st_writing");
      case "asking":
        return t("hc_st_asking");
      default:
        return t("hc_st_thinking");
    }
  }, [ph.what, ph.title]);
  const status = ended ? t("hc_st_ended") : !connected ? error ?? t("hc_st_connecting") : running ? null : chat.state === "parked" ? t("hc_st_parked") : null;

  return (
    <div className="nt-chat">
      <div className="nt-chat-hd">
        <i className={cx("dot", connected ? chat.state : "off")} />
        <div className="who">
          <span className="nm">{agent}</span>
          <span className="cwd mono" title={hello?.cwd}>
            {hello?.cwd ?? ""}
          </span>
        </div>
        {running && ph.since && <span className="hd-tm tnum">{secs(now - ph.since)}</span>}
        {readonly && <span className="cap c3">{t("hc_readonly")}</span>}
        <button type="button" className="ico" aria-label={t("hc_menu")} onClick={() => setMenu((v) => !v)}>
          <Icon name="more" size={14} />
        </button>
        {menu && (
          <div className="menu nt-chat-menu" role="menu">
            <button type="button" role="menuitem" onClick={() => { setMenu(false); onLeave(); }}>
              {t("hc_other")}
            </button>
            {!readonly && !ended && (
              <button type="button" role="menuitem" className="danger" onClick={() => { setMenu(false); send({ end: {} }); }}>
                {t("hc_end")}
              </button>
            )}
          </div>
        )}
      </div>
      {status && <div className={cx("nt-chat-status", !connected && !ended && error && "err")}>{status}</div>}
      <Transcript
        chat={chat}
        now={now}
        readonly={readonly}
        empty={t("hc_empty", { agent })}
        workingLabel={working}
        follow={follow}
        onAnswer={onAnswer}
        onContinue={onContinue}
      />
      <Composer
        agent={agent}
        host={host}
        running={running}
        canSteer={canSteer}
        disabled={ended ? "ended" : readonly ? "readonly" : null}
        context={chat.context}
        queued={chat.queued}
        history={history}
        onSend={sendText}
        onStop={() => send({ interrupt: {} })}
        onDequeue={(id) => send({ dequeue: { id } })}
        onEditQueued={(id, text) => {
          for (const c of editQueued(id, newId(), text)) send(c);
        }}
        onSteerQueued={(id, text) => {
          send({ dequeue: { id } });
          send(inputCommand(newId(), text, "steer"));
        }}
      />
    </div>
  );
}
