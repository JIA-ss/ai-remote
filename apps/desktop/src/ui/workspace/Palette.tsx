// The ⌘K panel (SB-05 / SB-06): searches every computer's sessions by title, folder, branch and
// computer name, every word having to match; sessions whose title matches come first. With nothing
// typed it lists the latest sessions. ↑ / ↓ move, Enter opens, Esc closes.

import { Search } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { folderName } from "../../lib/chat";
import { t } from "../../lib/i18n";
import { searchSessions, type WsItem, type WsSnapshot } from "../../lib/workspace";
import { cx } from "../util";
import { StatusDot } from "./parts";
import { titleOf } from "./Sidebar";
import { workspace } from "./useWorkspace";

/** Rows listed with nothing typed. */
const RECENT = 12;

export function Palette({ snap, onOpen, onClose }: { snap: WsSnapshot; onOpen: (x: WsItem) => void; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const items = useMemo(() => workspace.items(), [snap]);
  const results = useMemo(() => {
    const found = searchSessions(items, query, (x) => ({ title: titleOf(x), folder: x.meta.cwd, branch: x.meta.branch ?? "", computer: x.host }));
    return query.trim() ? found : found.slice(0, RECENT);
  }, [items, query]);
  const at = Math.min(active, Math.max(results.length - 1, 0));
  useEffect(() => input.current?.focus(), []);
  useEffect(() => {
    list.current?.querySelector<HTMLElement>(`[data-i="${at}"]`)?.scrollIntoView({ block: "nearest" });
  }, [at]);
  return (
    <div className="ws-scrim" onPointerDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="ws-palette" role="dialog" aria-modal="true" aria-label={t("ws_palette")}>
        <div className="ws-palette-q">
          <Search size={16} strokeWidth={1.5} />
          <input
            ref={input}
            value={query}
            placeholder={t("ws_search_ph")}
            aria-label={t("ws_palette")}
            role="combobox"
            aria-expanded="true"
            aria-controls="ws-palette-list"
            aria-activedescendant={results.length ? `ws-p-${at}` : undefined}
            spellCheck={false}
            autoComplete="off"
            onChange={(e) => {
              setQuery(e.target.value);
              setActive(0);
            }}
            onKeyDown={(e) => {
              if (e.nativeEvent.isComposing) return;
              if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                onClose();
              } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
                e.preventDefault();
                const n = results.length;
                if (n) setActive((at + (e.key === "ArrowDown" ? 1 : n - 1)) % n);
              } else if (e.key === "Enter" && results[at]) {
                e.preventDefault();
                onOpen(results[at]);
              }
            }}
          />
        </div>
        <div className="ws-palette-list" id="ws-palette-list" role="listbox" ref={list}>
          {results.length > 0 && <div className="ws-palette-h">{query.trim() ? t("ws_search_results") : t("ws_search_recent")}</div>}
          {results.length === 0 && <div className="ws-palette-none">{t("ws_search_none")}</div>}
          {results.map((x, i) => {
            const dot = workspace.dot(x);
            return (
              <button
                key={x.key}
                id={`ws-p-${i}`}
                data-i={i}
                type="button"
                role="option"
                aria-selected={i === at}
                className={cx("ws-palette-row", i === at && "on")}
                onMouseMove={() => i !== at && setActive(i)}
                onClick={() => onOpen(x)}
              >
                <span className="ws-palette-dot">{dot && <StatusDot kind={dot} />}</span>
                <span className="ws-palette-title">{titleOf(x)}</span>
                <span className="ws-palette-where">
                  {[folderName(x.meta.cwd), x.local ? null : x.host, x.meta.branch].filter(Boolean).join(" · ")}
                </span>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}
