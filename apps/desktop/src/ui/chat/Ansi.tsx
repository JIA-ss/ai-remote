// Opt-in terminal rendering shared by tool output and Markdown; plain React text nodes only.
import { memo, useMemo, type CSSProperties } from "react";
import { ansiOutput, type AnsiRun } from "../../lib/terminal";

export function ansiStyle(r: AnsiRun): CSSProperties {
  return {
    color: r.fg, backgroundColor: r.bg, fontWeight: r.bold ? 700 : undefined, opacity: r.dim ? 0.7 : undefined,
    fontStyle: r.italic ? "italic" : undefined,
    textDecoration: [r.underline && "underline", r.strike && "line-through"].filter(Boolean).join(" ") || undefined,
  };
}

const AnsiText = memo(function AnsiText({ text }: { text: string }) {
  const runs = useMemo(() => ansiOutput(text), [text]);
  return <>{runs.map((run, i) => <span key={i} style={ansiStyle(run)}>{run.text}</span>)}</>;
});

/** A stable renderer that Markdown callers can inject without changing other conversation views. */
export const renderAnsiText = (text: string) => <AnsiText text={text} />;
