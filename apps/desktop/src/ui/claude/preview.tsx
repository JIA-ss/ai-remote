// The Claude view alone against the demo backend (claude-preview.html, dev server only): the whole page
// width or the rail's, in either theme and language, for screenshots and design review. `start=<runtime>`
// starts a new demo session of that agent first (e.g. `opencode`, which queues messages sent mid-turn).

import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import "../../index.css";
import { api } from "../../lib/api";
import { applyPlatform, setThemeChoice } from "../../lib/platform";
import type { ChatTarget } from "../AgentChat";
import { ClaudeConversation } from "./ClaudeConversation";

const q = new URLSearchParams(location.search);
applyPlatform();
const theme = q.get("theme");
if (theme === "light" || theme === "dark") setThemeChoice(theme);
const variant = q.get("variant") === "rail" ? "rail" : "page";
const width = Number(q.get("w")) || (variant === "rail" ? 352 : 0);
const host = "office-pc";

function Preview() {
  const [target, setTarget] = useState<ChatTarget | null>(() => (q.get("start") ? null : { session: q.get("session") ?? "c3f9a1e7b2c4d" }));
  useEffect(() => {
    const runtime = q.get("start");
    if (!runtime) return;
    void api.callTool(host, "chat_start", { runtime, cwd: "D:\\work\\report-tool" }, true).then((r) => {
      const v = (r.structuredContent ?? JSON.parse(r.content.find((c) => c.type === "text")?.text ?? "{}")) as { session: string; ticket: string };
      setTarget({ session: v.session, ticket: v.ticket });
    });
  }, []);
  return (
    <div style={{ display: "flex", height: "100vh", justifyContent: "flex-end", background: "var(--layer)" }}>
      <div className={variant === "rail" ? "nt-rail" : undefined}
        style={{ display: "flex", flexDirection: "column", minHeight: 0, background: "var(--canvas)", ...(width ? { width, flex: "none" } : { flex: 1 }) }}>
        {target && <ClaudeConversation host={host} target={target} onLeave={() => {}} variant={variant} />}
      </div>
    </div>
  );
}

createRoot(document.getElementById("root") as HTMLElement).render(<Preview />);
