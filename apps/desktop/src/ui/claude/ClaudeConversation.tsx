// A Claude (or ACP agent) session's conversation the way Claude's own app shows it (docs/agent-workspace.zh-CN.md
// §2.3): the transcript in one column, the status line at its end, then a dock with the card the agent
// waits on (approval, plan, question), the task list, and the composer with the permission mode and the
// context ring under it. `variant="page"` is the workspace's whole page (its frame has the title bar);
// `variant="rail"` is the narrow chat tab beside the live view, with the panel's own header. A Codex
// session goes to the chat panel's Codex layout once the host names the agent (one connection either way).

import { Search } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  answerCommand,
  editQueued,
  inputCommand,
  markAnswered,
  markResolved,
  modeCommand,
  permitCommand,
  rememberSent,
  type InputMode,
} from "../../lib/chat";
import { quoteText, rememberImages } from "../../lib/chat-input";
import { isCodexRuntime } from "../../lib/codex";
import { STRAY_ENTER_MS, type Mode } from "../../lib/claude";
import { t } from "../../lib/i18n";
import { ConversationPanel, newId, sentBySession, useConversation, useNowTick, type ChatTarget, type Conv } from "../AgentChat";
import { Composer, type ComposerImage } from "../chat/Composer";
import { FolderLabel } from "../chat/FolderPicker";
import { PreviewProvider } from "../chat/Preview";
import { Icon } from "../Icon";
import { cx } from "../util";
import { CardDock, type CardActions } from "./Cards";
import { openStore } from "./ClaudeTurn";
import { ClaudeTranscript } from "./ClaudeTranscript";
import { ModePicker, StatusLine, TodoDock } from "./Dock";

export function ClaudeConversation({ host, target, onLeave, variant = "page" }: { host: string; target: ChatTarget; onLeave: (gone?: boolean) => void; variant?: "page" | "rail" }) {
  const conv = useConversation(host, target);
  if (isCodexRuntime(conv.hello?.runtime)) return <ConversationPanel host={host} target={target} onLeave={onLeave} conv={conv} />;
  return <ClaudeView host={host} target={target} onLeave={onLeave} variant={variant} conv={conv} />;
}

function ClaudeView({ host, target, onLeave, variant, conv }: { host: string; target: ChatTarget; onLeave: (gone?: boolean) => void; variant: "page" | "rail"; conv: Conv }) {
  const { chat, setChat, hello, error, connected, gone, send, outgoing, sendInput, retry, discard, listFiles } = conv;
  useEffect(() => {
    if (gone) onLeave(true);
  }, [gone]);
  const [menu, setMenu] = useState(false);
  const [finding, setFinding] = useState(false);
  const [findFocus, setFindFocus] = useState(0);
  const [insert, setInsert] = useState<{ text: string; n: number } | null>(null);
  const [history, setHistory] = useState<string[]>(() => sentBySession.get(target.session) ?? []);
  const [follow, setFollow] = useState(0);
  const [draft, setDraft] = useState(false);
  /** Stop was pressed: the status line says so until the turn ends. */
  const [stopping, setStopping] = useState(false);
  const holdEnter = useRef(0);
  const root = useRef<HTMLDivElement>(null);
  const store = useMemo(() => openStore(target.session), [target.session]);
  const running = chat.state === "running" || chat.state === "starting" || chat.state === "interrupting";
  const now = useNowTick(running || outgoing.some((o) => o.state === "sending"));
  const readonly = hello?.role === "ro";
  const ended = chat.state === "ended";
  const agent = hello?.runtime.name ?? t("hc_agent");
  const claude = hello?.runtime.protocol === "claude-stream-json" || hello?.runtime.id === "claude";
  const permissions = !!hello?.caps?.permissions;
  useEffect(() => {
    if (!running) setStopping(false);
  }, [running]);
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
  const onContinue = useCallback(() => {
    sendRef.current(inputCommand(newId(), t("hc_continue_prompt")));
    setFollow((n) => n + 1);
  }, []);
  const stop = () => {
    setStopping(true);
    send({ interrupt: {} });
  };
  /** After a card is answered the composer has the focus again; the same Enter press must not send (CP-06). */
  const backToComposer = () => {
    holdEnter.current = Date.now() + STRAY_ENTER_MS;
    root.current?.querySelector<HTMLTextAreaElement>(".nt-compose textarea")?.focus();
  };
  const actions: CardActions = {
    permit: (r, decision, message) => {
      send(permitCommand(r.id, decision, { message }));
      setChat((c) => markResolved(c, r.id, decision === "deny" ? "denied" : decision === "session" ? "allowed_session" : "allowed"));
      backToComposer();
    },
    plan: (r, how) => {
      send("mode" in how ? permitCommand(r.id, "once", { mode: how.mode }) : permitCommand(r.id, "deny", { message: how.revise }));
      setChat((c) => markResolved(c, r.id, "mode" in how ? "allowed" : "denied"));
      backToComposer();
    },
    answer: (q, answers) => {
      const parts = q.questions?.length ? q.questions : [{ question: q.prompt }];
      const first = answers[parts[0].question] ?? "";
      send(answerCommand(q.qid, first, answers));
      setChat((c) => markResolved(markAnswered(c, q.qid, first, answers), q.qid, "answered"));
      setFollow((n) => n + 1);
      backToComposer();
    },
  };
  const status = ended ? t("hc_st_ended") : !connected ? error ?? t("hc_st_connecting") : chat.state === "parked" ? t("hc_st_parked") : null;
  const sending = outgoing.some((o) => o.state === "sending");
  const cards = !ended && (chat.requests.some((r) => !r.resolved) || chat.turns.some((tn) => tn.status === "running" && tn.blocks.some((b) => b.kind === "question" && !b.answered && !b.outcome)));

  return (
    <div
      ref={root}
      className={cx("nt-chat cl-root", variant)}
      data-variant={variant}
      onKeyDownCapture={(e) => {
        if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "f") {
          e.preventDefault();
          e.stopPropagation();
          setFinding(true);
          setFindFocus((n) => n + 1);
        }
      }}
    >
      {variant === "rail" && (
        <div className="nt-chat-hd">
          <i className={cx("dot", connected ? chat.state : "off")} />
          <div className="who">
            <span className="nm">{chat.title?.text ?? agent}</span>
            {hello?.cwd ? <FolderLabel host={host} path={hello.cwd} className="cwd" /> : <span className="cwd" />}
          </div>
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
      )}
      {status && <div className={cx("nt-chat-status", !connected && !ended && error && "err")}>{status}</div>}
      <PreviewProvider host={host} session={target.session}>
        <ClaudeTranscript
          chat={chat}
          now={now}
          readonly={readonly}
          empty={t("hc_empty", { agent })}
          loading={!hello || chat.seq < hello.seq}
          follow={follow}
          outgoing={outgoing}
          store={store}
          onRetry={retry}
          onDiscard={discard}
          onContinue={onContinue}
          finding={finding}
          findFocus={findFocus}
          onCloseFind={() => setFinding(false)}
          onQuote={(text) => setInsert((q) => ({ text: quoteText(text), n: (q?.n ?? 0) + 1 }))}
          status={<StatusLine chat={chat} connected={connected} sending={sending} stopping={stopping} now={now} agent={agent} />}
        />
      </PreviewProvider>
      <div className="cl-dock">
        <TodoDock chat={chat} />
        {cards && <CardDock chat={chat} agent={agent} readonly={readonly} draft={draft} actions={actions} />}
        <Composer
          variant="claude"
          agent={agent}
          host={host}
          running={running}
          canSteer={!!hello?.caps?.steer}
          disabled={ended ? "ended" : readonly ? "readonly" : null}
          context={chat.context}
          queued={chat.queued}
          history={history}
          onSend={sendText}
          onStop={stop}
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
          onDraft={setDraft}
          holdEnter={holdEnter}
          escClaimed={cards}
          lead={claude && permissions ? (
            <ModePicker mode={chat.mode ?? hello?.permission_mode ?? null} busy={running} agent={agent} host={host} folder={hello?.cwd ?? ""} disabled={readonly || ended}
              onPick={(m: Mode) => send(modeCommand(m))} />
          ) : null}
        />
      </div>
    </div>
  );
}
