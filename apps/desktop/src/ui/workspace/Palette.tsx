// The ⌘K panel (SB-05 / SB-06): searches every computer's sessions by title (and the titles they had
// before, shown as "Formerly: …"), folder, branch and computer name, every word having to match;
// sessions whose title matches come first. From three characters on it also asks each online computer
// what was said (`chat_search`), 150 ms after the last key, and lists those sessions "In
// conversations" with an excerpt; a small spinner shows if that takes over 150 ms. With nothing typed
// it lists the latest sessions. ↑ / ↓ move, Enter opens, Esc closes.

import { Search, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { folderName } from "../../lib/chat";
import { t } from "../../lib/i18n";
import {
  contentHits,
  deepQuery,
  DEEP_SEARCH,
  formerTitle,
  searchSessions,
  type ContentHit,
  type WsItem,
  type WsSnapshot,
} from "../../lib/workspace";
import { cx } from "../util";
import { StatusDot, useReturnFocus } from "./parts";
import { titleOf } from "./Sidebar";
import { workspace } from "./useWorkspace";

/** Rows listed with nothing typed. */
const RECENT = 12;

/** What the hosts said of one query. */
interface Deep {
  query: string;
  hits: ContentHit[];
  truncated: boolean;
  done: boolean;
}

export function Palette({ snap, onOpen, onClose }: { snap: WsSnapshot; onOpen: (x: WsItem) => void; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  useReturnFocus(panel);
  const items = useMemo(() => workspace.items(), [snap]);
  const fields = (x: WsItem) => ({ title: titleOf(x), folder: x.meta.cwd, branch: x.meta.branch ?? "", computer: x.host, previous: x.meta.previous_titles });
  const results = useMemo(() => {
    const found = searchSessions(items, query, fields);
    return query.trim() ? found : found.slice(0, RECENT);
  }, [items, query]);
  const [deep, setDeep] = useState<Deep | null>(null);
  const [spinner, setSpinner] = useState(false);
  // The hosts' search of what was said: 150 ms after the last key, every online computer at once.
  const online = useMemo(() => snap.computers.filter((c) => c.online === true && snap.hosts[c.name]?.status !== "unsupported").map((c) => c.name), [snap]);
  const onlineKey = online.join("\n");
  useEffect(() => {
    const q = query.trim();
    if (!deepQuery(q) || !online.length) {
      setDeep(null);
      setSpinner(false);
      return;
    }
    let gone = false;
    const shown = setTimeout(() => !gone && setSpinner(true), DEEP_SEARCH.DEBOUNCE_MS + DEEP_SEARCH.SPINNER_MS);
    const start = setTimeout(() => {
      void Promise.all(
        online.map((host) =>
          workspace.search(host, q).then(
            (found) => ({ host, found }),
            // A host without chat_search (older) or that refused: nothing from it.
            () => ({ host, found: null }),
          ),
        ),
      ).then((answers) => {
        if (gone) return;
        setSpinner(false);
        const truncated = answers.some((a) => (a.found as { truncated?: unknown } | null)?.truncated === true);
        setDeep({ query: q, hits: contentHits(answers, new Set()), truncated, done: true });
      });
    }, DEEP_SEARCH.DEBOUNCE_MS);
    return () => {
      gone = true;
      clearTimeout(start);
      clearTimeout(shown);
    };
  }, [query, onlineKey]);
  // What was said, without the sessions already listed above.
  const content = useMemo(() => {
    if (!deep || deep.query !== query.trim()) return [];
    const listed = new Set(results.map((x) => x.key));
    return deep.hits.filter((h) => !listed.has(h.key));
  }, [deep, results, query]);
  const total = results.length + content.length;
  const at = Math.min(active, Math.max(total - 1, 0));
  useEffect(() => input.current?.focus(), []);
  useEffect(() => {
    list.current?.querySelector<HTMLElement>(`[data-i="${at}"]`)?.scrollIntoView({ block: "nearest" });
  }, [at, query, total]);
  const openHit = (h: ContentHit) => onOpen(workspace.find(h.host, h.meta.id) ?? { host: h.host, key: h.key, meta: h.meta, local: false, online: true });
  const pick = (i: number) => (i < results.length ? onOpen(results[i]) : content[i - results.length] && openHit(content[i - results.length]));
  const searching = spinner && deepQuery(query) && !(deep && deep.query === query.trim());
  return (
    <div className="ws-scrim" onPointerDown={(e) => e.target === e.currentTarget && onClose()}>
      <div ref={panel} className="ws-palette" role="dialog" aria-modal="true" aria-label={t("ws_palette")}
        onKeyDown={(e) => {
          if (e.nativeEvent.isComposing) return;
          if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); onClose(); }
          else if (e.key === "Tab") {
            const controls = panel.current?.querySelectorAll<HTMLElement>("input, button:not([tabindex='-1'])");
            if (!controls?.length) return;
            if (document.activeElement === (e.shiftKey ? controls[0] : controls[controls.length - 1])) {
              e.preventDefault();
              (e.shiftKey ? controls[controls.length - 1] : controls[0]).focus();
            }
          }
        }}>

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
            aria-activedescendant={total ? `ws-p-${at}` : undefined}
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
                if (total) setActive((at + (e.key === "ArrowDown" ? 1 : total - 1)) % total);
              } else if (e.key === "Enter" && total) {
                e.preventDefault();
                pick(at);
              }
            }}
          />
          <button type="button" className="ws-ib ws-palette-close" aria-label={t("hc_preview_close")} title={t("hc_preview_close")} onClick={onClose}>
            <X size={15} strokeWidth={1.5} />
          </button>
        </div>
        <div className="ws-palette-list" id="ws-palette-list" role="listbox" ref={list}>
          {results.length > 0 && <div className="ws-palette-h">{query.trim() ? t("ws_search_results") : t("ws_search_recent")}</div>}
          {total === 0 && !searching && <div className="ws-palette-none">{t("ws_search_none")}</div>}
          {results.map((x, i) => {
            const dot = workspace.dot(x);
            const former = query.trim() ? formerTitle(query, titleOf(x), x.meta.previous_titles) : null;
            return (
              <button
                key={x.key}
                id={`ws-p-${i}`}
                data-i={i}
                type="button"
                tabIndex={-1}
                role="option"
                aria-selected={i === at}
                className={cx("ws-palette-row", i === at && "on", former && "two")}
                onMouseMove={() => i !== at && setActive(i)}
                onPointerDown={(e) => e.preventDefault()}
                onClick={() => onOpen(x)}
              >
                <span className="ws-palette-dot">{dot && <StatusDot kind={dot} />}</span>
                {former ? (
                  <span className="ws-palette-main">
                    <span className="ws-palette-top">
                      <span className="ws-palette-title">{titleOf(x)}</span>
                      <span className="ws-palette-where">{where(x)}</span>
                    </span>
                    <span className="ws-palette-sub" data-formerly>
                      {t("ws_formerly", { title: former })}
                    </span>
                  </span>
                ) : (
                  <>
                    <span className="ws-palette-title">{titleOf(x)}</span>
                    <span className="ws-palette-where">{where(x)}</span>
                  </>
                )}
              </button>
            );
          })}
          {(content.length > 0 || searching) && (
            <div className="ws-palette-h deep" data-content-head>
              <span>{searching && !content.length ? t("ws_search_deeper") : t("ws_search_content")}</span>
              {searching && <span className="ws-spin" role="status" aria-label={t("ws_search_deeper")} />}
            </div>
          )}
          {content.map((h, j) => {
            const i = results.length + j;
            const x = workspace.find(h.host, h.meta.id);
            const title = x ? titleOf(x) : h.meta.title?.trim() || h.meta.first_text?.trim() || t("ws_untitled");
            const e = h.excerpts[h.excerpts.length - 1];
            return (
              <button
                key={h.key}
                id={`ws-p-${i}`}
                data-i={i}
                data-content-hit={h.key}
                type="button"
                tabIndex={-1}
                role="option"
                aria-selected={i === at}
                className={cx("ws-palette-row two", i === at && "on")}
                onMouseMove={() => i !== at && setActive(i)}
                onPointerDown={(e) => e.preventDefault()}
                onClick={() => openHit(h)}
              >
                <span className="ws-palette-dot">{x && workspace.dot(x) && <StatusDot kind={workspace.dot(x)!} />}</span>
                <span className="ws-palette-main">
                  <span className="ws-palette-top">
                    <span className="ws-palette-title">{title}</span>
                    <span className="ws-palette-where">{where(x ?? { host: h.host, key: h.key, meta: h.meta, local: false, online: true })}</span>
                  </span>
                  {e && (
                    <span className="ws-palette-sub" title={e.text}>
                      <b>{e.role === "user" ? t("ws_excerpt_you") : h.meta.name || h.meta.runtime}</b> {e.text}
                    </span>
                  )}
                </span>
              </button>
            );
          })}
          {deep?.truncated && deep.query === query.trim() && <div className="ws-palette-note">{t("ws_search_truncated")}</div>}
        </div>
      </div>
    </div>
  );
}

function where(x: WsItem): string {
  return [folderName(x.meta.cwd), x.local ? null : x.host, x.meta.branch].filter(Boolean).join(" · ");
}
