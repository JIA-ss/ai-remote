// The chat panel beside the live view: a coding agent running on the host (Claude Code, Codex, ...),
// started from here or from the computer page, and the conversation with it. Events come from the
// host's chat channel and are folded by lib/chat.ts; text from the agent is only ever rendered as
// text nodes (never HTML), and links are not followed from here.

import { useEffect, useMemo, useRef, useState } from "react";
import { api, type ChatConnection } from "../lib/api";
import {
  diffLines,
  diffStat,
  emptyChat,
  inlineParts,
  markAnswered,
  phase,
  reduceChat,
  rememberCwd,
  recentCwds,
  splitTurn,
  textParts,
  type Block,
  type Chat,
  type ChatEvent,
  type ChatHello,
  type Runtime,
  type RuntimesReport,
  type SessionInfo,
  type ToolItem,
  type Turn,
} from "../lib/chat";
import { t } from "../lib/i18n";
import { Icon, type IconName } from "./Icon";
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

/** Text with fenced code blocks and `inline code`, as text nodes. */
function Prose({ text }: { text: string }) {
  return (
    <>
      {textParts(text).map((p, i) =>
        p.code ? (
          <pre key={i} className="nt-chat-code">
            {p.text}
          </pre>
        ) : (
          <p key={i}>
            {inlineParts(p.text.trim()).map((s, j) => (s.code ? <code key={j}>{s.text}</code> : <span key={j}>{s.text}</span>))}
          </p>
        ),
      )}
    </>
  );
}

const KIND_ICON: Record<string, IconName> = {
  execute: "terminal",
  read: "eye",
  edit: "copy",
  delete: "trash",
  move: "copy",
  search: "search",
  fetch: "globe",
  think: "info",
  other: "wrench",
};

function secs(ms: number) {
  return ms < 1000 ? `${ms} ms` : ms < 60_000 ? `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)} s` : `${Math.floor(ms / 60_000)}:${String(Math.round((ms % 60_000) / 1000)).padStart(2, "0")}`;
}

function ToolRow({ x, now }: { x: ToolItem; now: number }) {
  const [open, setOpen] = useState(false);
  const stat = diffStat(x.content);
  const running = x.status === "in_progress" || x.status === "pending";
  const took = (x.endedTs ?? now) - x.startedTs;
  const detail = x.content.length > 0 || (x.input !== undefined && x.tool !== "execute");
  return (
    <div className={cx("nt-tool", x.status, open && "open")}>
      <button type="button" className="hd" onClick={() => detail && setOpen((v) => !v)} aria-expanded={open} disabled={!detail}>
        <span className="ic">
          <Icon name={KIND_ICON[x.tool] ?? "wrench"} size={12} />
        </span>
        <span className="ttl mono">{x.title}</span>
        {stat && (
          <span className="stat mono">
            <span className="add">+{stat.add}</span> <span className="del">−{stat.del}</span>
          </span>
        )}
        <span className="st">{running ? <i className="spin" /> : x.status === "failed" ? <Icon name="x" size={12} /> : <Icon name="check" size={12} />}</span>
        <span className="tm tnum">{running || took >= 100 ? secs(Math.max(took, 0)) : ""}</span>
      </button>
      {open && (
        <div className="bd">
          {x.input !== undefined && x.tool !== "execute" && !x.content.some((c) => c.type === "diff") && (
            <pre className="nt-chat-code small">{JSON.stringify(x.input, null, 2)}</pre>
          )}
          {x.content.map((c, i) =>
            c.type === "diff" ? (
              <div key={i} className="nt-diff">
                <div className="path mono">{c.path}</div>
                <pre>
                  {diffLines(c.old, c.new).map((l, j) => (
                    <span key={j} className={cx(l.sign === "+" ? "add" : l.sign === "-" ? "del" : "ctx")}>
                      {l.sign} {l.text}
                      {"\n"}
                    </span>
                  ))}
                </pre>
              </div>
            ) : (
              <pre key={i} className="nt-chat-code small">
                {c.text}
              </pre>
            ),
          )}
          {x.truncated && <div className="cut">{t("hc_cut")}</div>}
        </div>
      )}
    </div>
  );
}

function Thought({ b, now, running }: { b: Extract<Block, { kind: "thought" }>; now: number; running: boolean }) {
  const [open, setOpen] = useState(false);
  const took = (running ? now : b.endedTs) - b.startedTs;
  return (
    <div className={cx("nt-thought", open && "open")}>
      <button type="button" className="hd" onClick={() => setOpen((v) => !v)} aria-expanded={open}>
        <Icon name={open ? "chevD" : "chevR"} size={10} />
        {t("hc_thought", { s: secs(Math.max(took, 1000)) })}
      </button>
      {open && <div className="bd">{b.text}</div>}
    </div>
  );
}

function Question({ b, onAnswer, readonly }: { b: Extract<Block, { kind: "question" }>; onAnswer: (qid: string, text: string) => void; readonly: boolean }) {
  const [text, setText] = useState("");
  return (
    <div className={cx("nt-question", b.answered && "done")}>
      <div className="q">{b.prompt}</div>
      {!b.answered && !readonly && (
        <>
          {b.options.length > 0 && (
            <div className="opts">
              {b.options.map((o) => (
                <button key={o} type="button" className="btn sm" onClick={() => onAnswer(b.qid, o)}>
                  {o}
                </button>
              ))}
            </div>
          )}
          <div className="row">
            <input className="ta-input" value={text} placeholder={t("hc_answer_ph")} onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.nativeEvent.isComposing && e.keyCode !== 229 && text.trim()) onAnswer(b.qid, text.trim());
              }} />
          </div>
        </>
      )}
    </div>
  );
}

function BlockView({ b, now, running, onAnswer, readonly }: { b: Block; now: number; running: boolean; onAnswer: (qid: string, text: string) => void; readonly: boolean }) {
  switch (b.kind) {
    case "text":
      return (
        <div className="nt-msg">
          <Prose text={b.text} />
        </div>
      );
    case "thought":
      return <Thought b={b} now={now} running={running} />;
    case "tool":
      return <ToolRow x={b} now={now} />;
    case "plan":
      return (
        <ul className="nt-plan">
          {b.entries.map((e, i) => (
            <li key={i} className={e.status}>
              <i />
              {e.content}
            </li>
          ))}
        </ul>
      );
    case "question":
      return <Question b={b} onAnswer={onAnswer} readonly={readonly} />;
    case "notice":
      return <div className={cx("nt-chat-note", b.level === "error" ? "err" : b.level === "warn" ? "warn" : "")}>{b.text}</div>;
  }
}

function TurnView({ turn, now, onAnswer, readonly }: { turn: Turn; now: number; onAnswer: (qid: string, text: string) => void; readonly: boolean }) {
  const running = turn.status === "running";
  const [open, setOpen] = useState<boolean | null>(null);
  const { preamble, steps, answer } = splitTurn(turn);
  const tools = steps.filter((s) => s.kind === "tool").length;
  const expanded = open ?? running;
  return (
    <div className="nt-turn">
      {turn.user && (
        <div className="nt-user">
          <div className="bubble">{turn.user.text}</div>
        </div>
      )}
      {preamble.map((b, i) => (
        <BlockView key={`p${i}`} b={b} now={now} running={running} onAnswer={onAnswer} readonly={readonly} />
      ))}
      {steps.length > 0 && (
        <div className={cx("nt-steps", expanded && "open")}>
          <button type="button" className="hd" onClick={() => setOpen(!expanded)} aria-expanded={expanded}>
            <Icon name={expanded ? "chevD" : "chevR"} size={10} />
            {tools ? t("hc_steps", { n: tools }) : t("hc_thinking_only")}
          </button>
          {expanded && (
            <div className="bd">
              {steps.map((b, i) => (
                <BlockView key={`s${i}`} b={b} now={now} running={running && i === steps.length - 1} onAnswer={onAnswer} readonly={readonly} />
              ))}
            </div>
          )}
        </div>
      )}
      {answer.map((b, i) => (
        <BlockView key={`a${i}`} b={b} now={now} running={running} onAnswer={onAnswer} readonly={readonly} />
      ))}
      {turn.status !== "running" && (
        <div className={cx("nt-turn-ft", turn.status)}>
          {turn.status === "failed" ? t("hc_failed") : turn.status === "interrupted" ? t("hc_stopped") : null}
          {turn.reason && turn.status !== "completed" && <span className="why">{turn.reason}</span>}
          <span className="meta tnum">
            {turn.durationMs != null && secs(turn.durationMs)}
            {turn.usage?.output != null && ` · ${t("hc_tokens", { n: Math.round(((turn.usage.input ?? 0) + turn.usage.output) / 100) / 10 })}`}
          </span>
        </div>
      )}
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

function Conversation({ host, target, onLeave }: { host: string; target: ChatTarget; onLeave: () => void }) {
  const { chat, setChat, hello, error, connected, send } = useConversation(host, target);
  const [text, setText] = useState("");
  const [menu, setMenu] = useState(false);
  const scroll = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const running = chat.state === "running" || chat.state === "starting" || chat.state === "interrupting";
  const now = useNowTick(running);
  const readonly = hello?.role === "ro";
  const ended = chat.state === "ended";
  const ph = phase(chat);

  useEffect(() => {
    const el = scroll.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [chat]);

  const submit = (interrupt = false) => {
    const v = text.trim();
    if (!v) {
      if (running) send({ interrupt: {} });
      return;
    }
    const id = `u${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
    send({ input: { id, text: v, ...(interrupt ? { mode: "interrupt" } : {}) } });
    setText("");
    stick.current = true;
  };
  const answer = (qid: string, value: string) => {
    send({ answer: { qid, text: value } });
    setChat((c) => markAnswered(c, qid));
  };
  const status = useMemo(() => {
    if (ended) return t("hc_st_ended");
    if (!connected) return error ?? t("hc_st_connecting");
    switch (ph.what) {
      case "starting":
        return t("hc_st_starting");
      case "stopping":
        return t("hc_st_stopping");
      case "tool":
        return `${ph.title ?? ""}`;
      case "thinking":
        return t("hc_st_thinking");
      case "writing":
        return t("hc_st_writing");
      default:
        return chat.state === "parked" ? t("hc_st_parked") : t("hc_st_idle");
    }
  }, [ended, connected, error, ph.what, ph.title, chat.state]);

  return (
    <div className="nt-chat">
      <div className="nt-chat-hd">
        <i className={cx("dot", connected ? chat.state : "off")} />
        <div className="who">
          <span className="nm">{hello?.runtime.name ?? t("hc_agent")}</span>
          <span className="cwd mono" title={hello?.cwd}>
            {hello?.cwd ?? ""}
          </span>
        </div>
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
      <div
        className="nt-chat-body rail-scroll"
        ref={scroll}
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        }}
      >
        {chat.notices.map((n, i) => (
          <div key={`n${i}`} className={cx("nt-chat-note", n.level === "error" ? "err" : n.level === "warn" ? "warn" : "")}>
            {n.text}
          </div>
        ))}
        {chat.turns.length === 0 && !running && <div className="nt-chat-empty">{t("hc_empty", { agent: hello?.runtime.name ?? t("hc_agent") })}</div>}
        {chat.turns.map((turn) => (
          <TurnView key={turn.n} turn={turn} now={now} onAnswer={answer} readonly={readonly} />
        ))}
      </div>
      <div className="nt-chat-foot">
        <div className={cx("nt-chat-status", running && "busy")}>
          {running && <i className="spin" />}
          <span className="ell">{status}</span>
          {ph.since && running && <span className="tnum">{secs(now - ph.since)}</span>}
        </div>
        {chat.queued.length > 0 && (
          <div className="nt-queue">
            {chat.queued.map((q) => (
              <span key={q.id} className="chip">
                <span className="lbl">{t("hc_queued")}</span>
                <span className="ell">{q.text}</span>
                <button type="button" aria-label={t("hc_dequeue")} onClick={() => send({ dequeue: { id: q.id } })}>
                  <Icon name="x" size={10} />
                </button>
              </span>
            ))}
          </div>
        )}
        <div className={cx("composer nt-chat-compose", (ended || readonly) && "off")}>
          <textarea
            className="ph-input"
            rows={Math.min(6, Math.max(1, text.split("\n").length))}
            value={text}
            disabled={ended || readonly}
            placeholder={ended ? t("hc_ph_ended") : readonly ? t("hc_ph_readonly") : running ? t("hc_ph_running") : t("hc_ph", { agent: hello?.runtime.name ?? t("hc_agent"), host })}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              // An input method's Enter confirms the composition; it must not send.
              if (e.key !== "Enter" || e.shiftKey || e.nativeEvent.isComposing || e.keyCode === 229) return;
              e.preventDefault();
              submit(e.altKey);
            }}
          />
          {running && !text.trim() ? (
            <button type="button" className="send stop" onClick={() => send({ interrupt: {} })} aria-label={t("hc_stop")} title={t("hc_stop")}>
              <Icon name="stop" size={14} />
            </button>
          ) : (
            <button type="button" className="send" onClick={() => submit()} aria-label={t("send")} disabled={!text.trim() || ended || readonly}>
              <Icon name="send" size={16} />
            </button>
          )}
        </div>
        {running && text.trim() && (
          <div className="nt-chat-hint">
            {t("hc_hint_queue")}
            <button type="button" className="lnk" onClick={() => submit(true)}>
              {t("hc_interrupt_send")}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
