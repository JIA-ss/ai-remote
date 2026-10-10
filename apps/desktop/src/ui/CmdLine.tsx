import { useState } from "react";
import { t } from "../lib/i18n";
import { Icon } from "./Icon";

/** A command with a Copy button. */
export function CmdLine({ cmd, style, multiline = false }: { cmd: string; style?: React.CSSProperties; multiline?: boolean }) {
  const [done, setDone] = useState(false);
  const [error, setError] = useState(false);
  return (
    <div className={`cmdline${multiline ? " multiline" : ""}`} style={style}>
      <span className="mono">{cmd}</span>
      <button
        className="btn"
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(cmd);
            setError(false);
            setDone(true);
            setTimeout(() => setDone(false), 1500);
          } catch {
            setError(true);
          }
        }}
      >
        <Icon name={done ? "check" : "copy"} size={14} />
        {error ? t("copy_failed") : done ? t("copied") : t("copy")}
      </button>
    </div>
  );
}
