// The chat panel beside the live view: a coding agent running on the host (Claude Code, Codex, ...),
// started from here or from the computer page, and the conversation with it. Events come from the
// host's chat channel and are folded by lib/chat.ts; text from the agent is only ever rendered as
// text nodes (never HTML), and links are not followed from here. The conversation's look follows
// Orca's native chat (see ui/chat/).

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, type ChatConnection } from "../lib/api";
import {
  editQueued,
  agoParts,
  emptyChat,
  folderChoices,
  folderName,
  knownHome,
  parentFolder,
  sessionsByFolder,
  tildeFolder,
  inputCommand,
  markAnswered,
  outgoingDisconnected,
  outgoingReply,
  retryOutgoing,
  settleOutgoing,
  type Outgoing,
  phase,
  reduceChat,
  rememberCwd,
  chatUnsupported,
  sessionGone,
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
import { isCodexRuntime, pendingRequest } from "../lib/codex";
import { t } from "../lib/i18n";
import { quoteText, rememberImages } from "../lib/chat-input";
import { Search } from "lucide-react";
import { Composer, type ComposerImage } from "./chat/Composer";
import { FolderGlyph, FolderLabel, FolderPicker, type Listing } from "./chat/FolderPicker";
import { secs } from "./chat/Rows";
import { PreviewProvider } from "./chat/Preview";
import { Transcript } from "./chat/Transcript";
import { CodexLiveStrip, livePlan } from "./codex/CodexLiveStrip";
import { CodexRequestCard, requestKey } from "./codex/CodexRequestCard";
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
  const [state, setState] = useState<{ report: RuntimesReport | null; error: string | null; old?: boolean }>({ report: null, error: null });
  const [round, setRound] = useState(0);
  useEffect(() => {
    let live = true;
    call<RuntimesReport>(host, "chat_runtimes", { refresh: round > 0 }).then(
      (report) => live && setState({ report, error: null }),
      (e) => {
        if (!live) return;
        const msg = String(e instanceof Error ? e.message : e);
        // A host from before coding agents answers "unknown tool": say what to do, not the MCP error.
        if (chatUnsupported(msg)) setState({ report: null, error: t("hc_host_old", { host }), old: true });
        else setState({ report: null, error: msg });
      },
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
  folders,
  onStarted,
}: {
  host: string;
  report: RuntimesReport;
  compact?: boolean;
  /** Folders of the host's open sessions, offered after the recent ones. */
  folders?: string[];
  onStarted: (target: ChatTarget) => void;
}) {
  const ready = report.runtimes.filter((r) => r.verdict === "ok");
  const [runtime, setRuntime] = useState(ready[0]?.id ?? "");
  const choices = folderChoices(recentCwds(host), folders);
  const [cwd, setCwd] = useState(choices[0] ?? "");
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
      <label className="lbl">{t("hc_cwd")}</label>
      <FolderPicker
        host={host}
        value={cwd}
        folders={choices}
        browse={(path) => call<Listing>(host, "files_list", { path })}
        onChange={(path) => {
          setCwd(path);
          setError(null);
        }}
      />
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
    // The clock starts from now, not from when the panel mounted: a turn that starts later would
    // otherwise show "0 ms" until the first tick (or longer, where a hidden page slows timers).
    setNow(Date.now());
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
  /** The host no longer has this session: the panel goes back to the list. */
  const [gone, setGone] = useState(false);
  const conn = useRef<ChatConnection | null>(null);
  const chatRef = useRef(chat);
  chatRef.current = chat;
  /** Messages this device sent that the conversation does not show yet. */
  const [outgoing, setOutgoing] = useState<Outgoing[]>([]);
  useEffect(() => {
    if (!target) return;
    let live = true;
    let ticket = target.ticket;
    let retry: ReturnType<typeof setTimeout> | null = null;
    // Events are folded at most once per frame: a fast agent must not re-render per token while
    // the live view keeps the compositor busy.
    let pending: ChatEvent[] = [];
    // A hidden page gets no animation frames at all, so a timer flushes too (whichever comes first).
    let frame = 0;
    let fallback: ReturnType<typeof setTimeout> | null = null;
    const flush = () => {
      if (frame) cancelAnimationFrame(frame);
      if (fallback) clearTimeout(fallback);
      frame = 0;
      fallback = null;
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
          if (!frame) {
            frame = requestAnimationFrame(flush);
            fallback = setTimeout(flush, 100);
          }
        }, () => {
          if (!live) return;
          setConnected(false);
          setOutgoing(outgoingDisconnected);
          conn.current = null;
          retry = setTimeout(() => void open(), 3000);
        }, (m) => {
          if (live) setOutgoing((o) => outgoingReply(o, m));
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
        if (sessionGone(msg)) setGone(true);
        else if (!/requires level|no longer has access/.test(msg)) retry = setTimeout(() => void open(), 5000);
      }
    };
    void open();
    return () => {
      live = false;
      if (retry) clearTimeout(retry);
      if (frame) cancelAnimationFrame(frame);
      if (fallback) clearTimeout(fallback);
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
  // Delivered messages leave the list; unanswered ones fail after a while.
  const acks = !!hello?.acks;
  useEffect(() => setOutgoing((o) => settleOutgoing(o, chat, Date.now(), acks)), [chat, acks]);
  const anySending = outgoing.some((o) => o.state === "sending");
  useEffect(() => {
    if (!anySending) return;
    const id = setInterval(() => setOutgoing((o) => settleOutgoing(o, chatRef.current, Date.now(), acks)), 1000);
    return () => clearInterval(id);
  }, [anySending, acks]);
  /** Sends a message and shows it as on its way until the conversation has it. */
  const sendInput = (o: Outgoing) => {
    setOutgoing((list) => [...list.filter((x) => x.id !== o.id), o]);
    const fail = (error: string) => setOutgoing((list) => list.map((x) => (x.id === o.id ? { ...x, state: "failed" as const, error } : x)));
    if (!conn.current) {
      fail("disconnected");
      return;
    }
    conn.current.send(inputCommand(o.id, o.text, o.mode, o.attachments)).catch((e) => fail(e instanceof Error ? e.message : String(e)));
  };
  const retry = (o: Outgoing) => {
    setOutgoing((list) => list.filter((x) => x.id !== o.id));
    const again = retryOutgoing(o, newId(), Date.now());
    rememberImages(again.id, again.images ?? []);
    sendInput(again);
  };
  const discard = (id: string) => setOutgoing((list) => list.filter((x) => x.id !== id));
  /** Files in the session's folder matching `query` (the `@` menu). */
  const listFiles = async (query: string) => {
    if (!conn.current) throw new Error(t("hc_not_connected"));
    const r = await conn.current.ask("files", { query, limit: 50 });
    return { files: Array.isArray(r.files) ? (r.files as string[]) : [], truncated: !!r.truncated };
  };
  return { chat, setChat, hello, error, connected, gone, send, conn, outgoing, sendInput, retry, discard, listFiles };
}

/** The chat tab of the session page. */
export function AgentChat({ host, target, onTarget }: { host: string; target: ChatTarget | null; onTarget: (t: ChatTarget | null) => void }) {
  // Leaving re-renders here even when the parent's target was already null (the panel had opened
  // the remembered session), and says why when the host no longer had the session.
  const [left, setLeft] = useState<{ gone: boolean } | null>(null);
  const current = target ?? (lastSession(host) ? { session: lastSession(host) as string } : null);
  if (!current) return <Picker host={host} note={left?.gone ? t("hc_session_gone") : null} onPick={(p) => { setLeft(null); onTarget(p); }} />;
  return (
    <Conversation
      key={current.session}
      host={host}
      target={current}
      onLeave={(gone) => {
        rememberSession(host, null);
        setLeft({ gone: !!gone });
        onTarget(null);
      }}
    />
  );
}

/** "3 min ago" for a session's last activity. */
function ago(then: number): string {
  const a = agoParts(then, Date.now());
  return a.unit === "now" ? t("hc_folder_ago_now") : t(`hc_folder_ago_${a.unit}`, { n: a.n });
}

/** A session's state in a word, for the session list. */
function sessionState(state: SessionInfo["state"]): string {
  if (state === "running" || state === "starting" || state === "interrupting") return t("hc_folder_st_running");
  if (state === "parked") return t("hc_folder_st_parked");
  return t("hc_st_idle");
}

/** No session open: the sessions on the host, and starting a new one. */
function Picker({ host, note, onPick }: { host: string; note: string | null; onPick: (t: ChatTarget) => void }) {
  const { report, error, old } = useRuntimes(host);
  const sessions = useSessions(host);
  const open = (sessions ?? []).filter((s) => s.state !== "ended");
  const pick = (t: ChatTarget) => {
    rememberSession(host, t.session);
    onPick(t);
  };
  return (
    <div className="nt-chat-picker rail-scroll">
      {note && <div className="nt-chat-note warn">{note}</div>}
      {open.length > 0 && (
        <>
          <div className="grp-h">{t("hc_folder_projects")}</div>
          <div className="nt-sessions">
            {sessionsByFolder(open).map((g) => (
              <div key={g.folder} className="nt-proj">
                <div className="nt-proj-hd" title={g.folder}>
                  <FolderGlyph size={14} />
                  <span className="fn">{folderName(g.folder)}</span>
                  {parentFolder(g.folder) && (
                    <span className="fp mono">
                      <bdi>{tildeFolder(parentFolder(g.folder) as string, knownHome(host))}</bdi>
                    </span>
                  )}
                  <span className="n tnum">{g.sessions.length}</span>
                </div>
                {g.sessions.map((s) => (
                  <button key={s.id} type="button" className="sess" onClick={() => pick({ session: s.id })}>
                    <i className={cx("dot", s.state)} />
                    <span className="nm">{s.model ? `${s.name} · ${s.model}` : s.name}</span>
                    <span className="tm">{ago(s.last_activity)}</span>
                    <span className="meta">
                      {[sessionState(s.state), t("hc_turns", { n: s.turns }), s.by?.device].filter(Boolean).join(" · ")}
                    </span>
                  </button>
                ))}
              </div>
            ))}
          </div>
        </>
      )}
      <div className="grp-h">{t("hc_new")}</div>
      {error ? <div className={cx("nt-chat-note", old ? "warn" : "err")}>{error}</div> : report ? <StartForm host={host} report={report} compact folders={open.map((s) => s.cwd)} onStarted={pick} /> : <div className="nt-chat-note">{t("hc_looking", { host })}</div>}
    </div>
  );
}


const newId = () => `u${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;

/** Sent messages per session, for ↑ / ↓ (kept for this run of the app). */
const sentBySession = new Map<string, string[]>();

/**
 * One session's conversation: header, find, the transcript, the composer. Codex sessions lay their turns
 * out the way Codex's own app does (ui/codex); `CodexConversation` is the same panel for them.
 */
export function Conversation({ host, target, onLeave }: { host: string; target: ChatTarget; onLeave: (gone?: boolean) => void }) {
  const { chat, setChat, hello, error, connected, gone, send, outgoing, sendInput, retry, discard, listFiles } = useConversation(host, target);
  // A remembered session the host no longer has (it restarted): forget it and show the list.
  useEffect(() => {
    if (gone) onLeave(true);
  }, [gone]);
  const [menu, setMenu] = useState(false);
  const [finding, setFinding] = useState(false);
  const [findFocus, setFindFocus] = useState(0);
  const [insert, setInsert] = useState<{ text: string; n: number } | null>(null);
  const [history, setHistory] = useState<string[]>(() => sentBySession.get(target.session) ?? []);
  const [follow, setFollow] = useState(0);
  const running = chat.state === "running" || chat.state === "starting" || chat.state === "interrupting";
  const now = useNowTick(running);
  const readonly = hello?.role === "ro";
  const ended = chat.state === "ended";
  const agent = hello?.runtime.name ?? t("hc_agent");
  // Codex conversations are laid out the way Codex's own app shows them; others keep this panel's rows.
  const codex = isCodexRuntime(hello?.runtime);
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
  const sendText = (text: string, mode: InputMode | undefined, images: ComposerImage[] = []) => {
    const id = newId();
    const shown = images.map((i) => ({ name: i.payload.name, url: i.url }));
    rememberImages(id, shown);
    sendInput({ id, text, mode, at: Date.now(), state: "sending", images: shown, attachments: images.map((i) => i.payload) });
    if (text) remember(text);
    setFollow((n) => n + 1);
  };
  const onAnswer = useCallback((qid: string, value: string) => {
    sendRef.current({ answer: { qid, text: value } });
    setChat((c) => markAnswered(c, qid, value));
    setFollow((n) => n + 1);
  }, [setChat]);
  // Codex asks in a card in place of the composer; a request the reader closed (which stops the turn)
  // is not asked again while the stop takes effect.
  const pending = codex ? pendingRequest(chat) : null;
  const pendingId = pending ? requestKey(host, target.session, pending) : null;
  const [closed, setClosed] = useState<string | null>(null);
  const asking = pending && pendingId !== closed ? pending : null;
  const plan = codex && !asking ? livePlan(chat.turns) : null;
  const onAnswers = (request: string | null, answers: Array<[string, string]>) => {
    for (const [qid, text] of answers) sendRef.current({ answer: { qid, text } });
    setChat((c) => answers.reduce((acc, [qid, text]) => markAnswered(acc, qid, text, request ?? undefined), c));
    setFollow((n) => n + 1);
  };
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
    <div
      className={cx("nt-chat", codex && "cdx")}
      onKeyDownCapture={(e) => {
        // ⌘F / Ctrl+F finds in the conversation while the panel has the focus.
        if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "f") {
          e.preventDefault();
          e.stopPropagation();
          // Open, or (already open) back to its field; no animation frame: a hidden page has none.
          setFinding(true);
          setFindFocus((n) => n + 1);
        }
      }}
    >
      <div className="nt-chat-hd">
        <i className={cx("dot", connected ? chat.state : "off")} />
        <div className="who">
          <span className="nm">{agent}</span>
          {hello?.cwd ? <FolderLabel host={host} path={hello.cwd} className="cwd" /> : <span className="cwd" />}
        </div>
        {/* A Codex turn says itself how long it works (its "Working for" line). */}
        {running && ph.since && !codex && <span className="hd-tm tnum">{secs(now - ph.since)}</span>}
        {readonly && <span className="cap c3">{t("hc_readonly")}</span>}
        <button type="button" className="ico" aria-label={t("hc_find_open")} title={t("hc_find_open")} onClick={() => setFinding((v) => !v)}>
          <Search size={14} />
        </button>
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
      <PreviewProvider host={host} session={target.session}>
      <Transcript
        chat={chat}
        now={now}
        readonly={readonly}
        empty={t("hc_empty", { agent })}
        workingLabel={working}
        follow={follow}
        outgoing={outgoing}
        onRetry={retry}
        onDiscard={discard}
        onAnswer={onAnswer}
        onContinue={onContinue}
        finding={finding}
        findFocus={findFocus}
        onCloseFind={() => setFinding(false)}
        onQuote={(text) => setInsert((q) => ({ text: quoteText(text), n: (q?.n ?? 0) + 1 }))}
        variant={codex ? "codex" : "default"}
        session={target.session}
        cwd={hello?.cwd ?? null}
      />
      </PreviewProvider>
      {plan && <CodexLiveStrip plan={plan} />}
      {asking && pendingId && (
        <div className="cdx-reqs">
          <CodexRequestCard key={pendingId} req={asking} id={pendingId} readonly={readonly}
            onSubmit={(answers) => onAnswers(asking.request, answers)}
            onDismiss={() => {
              setClosed(pendingId);
              send({ interrupt: {} });
            }} />
        </div>
      )}
      <div className="nt-compose-slot" hidden={!!asking}>
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
        commands={chat.commands ?? hello?.commands ?? null}
        usage={chat.usage}
        insert={insert}
        listFiles={readonly ? undefined : listFiles}
        images={(chat.images ?? hello?.images ?? hello?.caps?.images ?? true) ? { ok: true } : { ok: false, why: t("hc_img_unsupported", { agent }) }}
        onSteerQueued={(id, text) => {
          send({ dequeue: { id } });
          send(inputCommand(newId(), text, "steer"));
        }}
      />
      </div>
    </div>
  );
}
