import { useState } from "react";
import { t } from "../lib/i18n";
import { Icon } from "./Icon";

/** A command with a Copy button. */
export function CmdLine({ cmd, style }: { cmd: string; style?: React.CSSProperties }) {
  const [done, setDone] = useState(false);
  return (
    <div className="cmdline" style={style}>
      <span className="mono">{cmd}</span>
      <button
        className="btn"
        onClick={() => {
          void navigator.clipboard?.writeText(cmd);
          setDone(true);
          setTimeout(() => setDone(false), 1500);
        }}
      >
        <Icon name={done ? "check" : "copy"} size={14} />
        {done ? t("copied") : t("copy")}
      </button>
    </div>
  );
}
