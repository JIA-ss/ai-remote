// The Claude view alone against the demo backend (claude-preview.html, dev server only): the whole page
// width or the rail's, in either theme and language, for screenshots and design review.

import { createRoot } from "react-dom/client";
import "../../index.css";
import { applyPlatform, setThemeChoice } from "../../lib/platform";
import { ClaudeConversation } from "./ClaudeConversation";

const q = new URLSearchParams(location.search);
applyPlatform();
const theme = q.get("theme");
if (theme === "light" || theme === "dark") setThemeChoice(theme);
const variant = q.get("variant") === "rail" ? "rail" : "page";
const width = Number(q.get("w")) || (variant === "rail" ? 352 : 0);
const session = q.get("session") ?? "c3f9a1e7b2c4d";

createRoot(document.getElementById("root") as HTMLElement).render(
  <div style={{ display: "flex", height: "100vh", justifyContent: "flex-end", background: "var(--layer)" }}>
    <div className={variant === "rail" ? "nt-rail" : undefined}
      style={{ display: "flex", flexDirection: "column", minHeight: 0, background: "var(--canvas)", ...(width ? { width, flex: "none" } : { flex: 1 }) }}>
      <ClaudeConversation host="office-pc" target={{ session }} onLeave={() => {}} variant={variant} />
    </div>
  </div>,
);
