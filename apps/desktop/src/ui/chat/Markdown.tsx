// Markdown, code blocks and the copy button of the chat panel.
//
// Layout and behaviour follow Orca's native chat (stablyai/orca, MIT License, Copyright (c) 2026
// Lovecast Inc.): NativeChatMarkdown, NativeChatCodeBlock (language header, wrap and copy actions,
// a fence without a language gets its actions on hover), NativeChatCopyButton and
// NativeChatHighlightedCode. Rewritten for this app: our own parser (lib/markdown.ts) and
// tokenizer (lib/highlight.ts), rendered as text nodes only, and styled by native.css.

import { Check, Copy, WrapText } from "lucide-react";
import { memo, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { highlight, languageOf } from "../../lib/highlight";
import { t } from "../../lib/i18n";
import { holdStreamingTail, parseInline, parseMarkdown, parseMarkdownStream, safeHref, type Inline, type MdBlock, type MdStream } from "../../lib/markdown";
import { fileTarget } from "../../lib/preview";
import { cx } from "../util";
import { usePreview } from "./preview-context";

async function writeClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** Copies `text`; the icon turns into a check for a moment. */
export function CopyButton({ text, label, className }: { text: string; label?: string; className?: string }) {
  const [done, setDone] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => {
    if (timer.current) clearTimeout(timer.current);
  }, []);
  const name = label ?? t("hc_copy");
  return (
    <button
      type="button"
      className={cx("nt-ib", className)}
      aria-label={done ? t("hc_copied") : name}
      title={done ? t("hc_copied") : name}
      onMouseDown={(e) => e.preventDefault()}
      onClick={(e) => {
        e.stopPropagation();
        void writeClipboard(text).then((ok) => {
          if (!ok) return;
          setDone(true);
          if (timer.current) clearTimeout(timer.current);
          timer.current = setTimeout(() => setDone(false), 1500);
        });
      }}
    >
      {done ? <Check size={13} /> : <Copy size={13} />}
    </button>
  );
}

/** Code colored in place; its text is unchanged, so copying and layout match the plain text. */
export const Highlighted = memo(function Highlighted({ code, lang }: { code: string; lang?: string }) {
  const tokens = useMemo(() => highlight(code, lang), [code, lang]);
  return <>{tokens.map((tk, i) => (tk.c ? <span key={i} className={`hl-${tk.c}`}>{tk.v}</span> : tk.v))}</>;
});

const LANG_LABEL: Record<string, string> = {
  js: "JavaScript", ts: "TypeScript", tsx: "TSX", jsx: "JSX", py: "Python", python: "Python", rs: "Rust", rust: "Rust", go: "Go",
  sh: "Shell", bash: "Bash", zsh: "Zsh", ps1: "PowerShell", powershell: "PowerShell", json: "JSON", toml: "TOML", yaml: "YAML", yml: "YAML",
  sql: "SQL", css: "CSS", html: "HTML", xml: "XML", diff: "Diff", c: "C", cpp: "C++", cs: "C#", java: "Java", swift: "Swift", kotlin: "Kotlin",
  md: "Markdown", markdown: "Markdown", text: "Text", txt: "Text",
};

/** A fenced code block: language, wrap and copy; colored when the language is one we know. */
type AnsiRenderer = (code: string) => ReactNode;
export function CodeBlock({ code, lang, className, renderAnsi }: { code: string; lang?: string; className?: string; renderAnsi?: AnsiRenderer }) {
  const terminal = !!renderAnsi && code.includes("\x1b");
  const [wrap, setWrap] = useState(terminal);
  // A streamed fence may receive its first escape only after mounting.
  useEffect(() => { if (terminal) setWrap(true); }, [terminal]);
  const known = languageOf(lang);
  const actions = (
    <>
      <button type="button" className={cx("nt-ib", wrap && "on")} aria-pressed={wrap} aria-label={wrap ? t("hc_unwrap") : t("hc_wrap")}
        title={wrap ? t("hc_unwrap") : t("hc_wrap")} onClick={() => setWrap((v) => !v)}>
        <WrapText size={13} />
      </button>
      <CopyButton text={code} label={t("hc_copy_code")} />
    </>
  );
  return (
    <div className={cx("nt-cb", !lang && "bare", className)}>
      {lang ? (
        <div className="rh">
          <span className="lang">{LANG_LABEL[lang] ?? lang}</span>
          <span className="acts">{actions}</span>
        </div>
      ) : (
        <span className="acts float">{actions}</span>
      )}
      <pre className={cx(wrap && "wrapped")}>
        <code>{terminal ? renderAnsi!(code) : known ? <Highlighted code={code} lang={lang} /> : code}</code>
      </pre>
    </div>
  );
}

function InlineView({ c, interactive = true }: { c: Inline[]; interactive?: boolean }): ReactNode {
  const preview = usePreview();
  return c.map((x, i) => {
    switch (x.t) {
      case "text":
        return x.v;
      case "code": {
        // `out/report.html` in a reply opens its preview.
        const file = interactive && preview && fileTarget(x.v);
        if (!file) return <code key={i}>{x.v}</code>;
        return (
          <code key={i} className="md-file" role="button" tabIndex={0} title={t("hc_preview_open", { name: x.v })}
            onClick={() => preview.open(file)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                preview.open(file);
              }
            }}>
            {x.v}
          </code>
        );
      }
      case "br":
        return <br key={i} />;
      case "em":
        return <em key={i}><InlineView c={x.c} interactive={interactive} /></em>;
      case "strong":
        return <strong key={i}><InlineView c={x.c} interactive={interactive} /></strong>;
      case "del":
        return <del key={i}><InlineView c={x.c} interactive={interactive} /></del>;
      case "link": {
        const href = safeHref(x.href);
        if (!interactive) return <span key={i} title={href ?? undefined}><InlineView c={x.c} interactive={false} /></span>;
        const file = !href && preview ? fileTarget(x.href) : null;
        if (file && preview) {
          return (
            <a key={i} className="md-a md-file" href="#" title={t("hc_preview_open", { name: file })}
              onClick={(e) => {
                e.preventDefault();
                preview.open(file);
              }}>
              <InlineView c={x.c} />
            </a>
          );
        }
        if (!href) return <span key={i}><InlineView c={x.c} /></span>;
        // Links are not followed from inside the app (the page would leave the panel); a click
        // copies the address, and hovering shows it.
        return (
          <a key={i} className="md-a" href={href} title={t("hc_link_copy", { url: href })}
            onClick={(e) => {
              e.preventDefault();
              void writeClipboard(href);
            }}>
            <InlineView c={x.c} />
          </a>
        );
      }
    }
  });
}

/** Inline formatting for control labels. Links and file names remain noninteractive inside buttons. */
export const InlineMarkdown = memo(function InlineMarkdown({ text }: { text: string }) {
  const c = useMemo<Inline[]>(() => /(?:^|\n) {0,3}(?:`{3}|~{3})/.test(text) ? [{ t: "text", v: text }] : parseInline(text), [text]);
  return <span className="md-inline"><InlineView c={c} interactive={false} /></span>;
});

function BlockView({ b, renderAnsi }: { b: MdBlock; renderAnsi?: AnsiRenderer }): ReactNode {
  switch (b.t) {
    case "p":
      return <p><InlineView c={b.c} /></p>;
    case "h": {
      const H = `h${Math.min(b.level + 2, 6)}` as "h3";
      return <H className={`md-h${b.level}`}><InlineView c={b.c} /></H>;
    }
    case "code":
      return <CodeBlock code={b.text} lang={b.lang || undefined} renderAnsi={renderAnsi} />;
    case "quote":
      return <blockquote><Blocks blocks={b.c} renderAnsi={renderAnsi} /></blockquote>;
    case "hr":
      return <hr />;
    case "list": {
      const items = b.items.map((it, i) => (
        <li key={i} className={cx(it.task !== null && "task")}>
          {it.task !== null && <input type="checkbox" checked={it.task} readOnly tabIndex={-1} aria-label={it.task ? t("hc_done") : t("hc_todo")} />}
          <Blocks blocks={it.c} tight renderAnsi={renderAnsi} />
        </li>
      ));
      return b.ordered ? <ol start={b.start}>{items}</ol> : <ul>{items}</ul>;
    }
    case "table":
      return (
        <div className="md-table">
          <table>
            <thead>
              <tr>{b.head.map((h, i) => <th key={i} style={{ textAlign: b.align[i] ?? undefined }}><InlineView c={h} /></th>)}</tr>
            </thead>
            <tbody>
              {b.rows.map((r, i) => (
                <tr key={i}>{r.map((cell, k) => <td key={k} style={{ textAlign: b.align[k] ?? undefined }}><InlineView c={cell} /></td>)}</tr>
              ))}
            </tbody>
          </table>
        </div>
      );
  }
}

function Blocks({ blocks, tight, renderAnsi }: { blocks: MdBlock[]; tight?: boolean; renderAnsi?: AnsiRenderer }) {
  // A tight list item is one paragraph without a <p> margin.
  if (tight && blocks.length >= 1 && blocks[0].t === "p") {
    return (
      <>
        <InlineView c={blocks[0].c} />
        {blocks.slice(1).map((b, i) => <BlockView key={i} b={b} renderAnsi={renderAnsi} />)}
      </>
    );
  }
  return <>{blocks.map((b, i) => <BlockView key={i} b={b} renderAnsi={renderAnsi} />)}</>;
}

/** An agent's message as markdown. Parsed again only when its text changes. */
export const Markdown = memo(function Markdown({ text, className, renderAnsi }: { text: string; className?: string; renderAnsi?: AnsiRenderer }) {
  const blocks = useMemo(() => parseMarkdown(text), [text]);
  return (
    <div className={cx("nt-md", className)}>
      <Blocks blocks={blocks} renderAnsi={renderAnsi} />
    </div>
  );
});

/** A block of a reply still streaming: the blocks before the last safe cut keep their objects, so they
 *  do not render again as the text grows (lib/markdown.ts `parseMarkdownStream`). */
const StreamBlock = memo(function StreamBlock({ b, renderAnsi }: { b: MdBlock; renderAnsi?: AnsiRenderer }) {
  return <BlockView b={b} renderAnsi={renderAnsi} />;
});

/** An agent's message while it streams: only its tail is parsed again per update, and a last line that
 *  is only a list or heading marker waits for its words. Finished, it renders like `Markdown`. */
export const StreamingMarkdown = memo(function StreamingMarkdown({ text, streaming, className, renderAnsi }: { text: string; streaming: boolean; className?: string; renderAnsi?: AnsiRenderer }) {
  const prev = useRef<MdStream | null>(null);
  const blocks = useMemo(() => {
    const next = parseMarkdownStream(prev.current, streaming ? holdStreamingTail(text) : text);
    prev.current = next;
    return next.blocks;
  }, [text, streaming]);
  return (
    <div className={cx("nt-md", className)}>
      {blocks.map((b, i) => <StreamBlock key={i} b={b} renderAnsi={renderAnsi} />)}
    </div>
  );
});
